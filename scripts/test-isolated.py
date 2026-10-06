#!/usr/bin/env python3
"""Run Rust suites with fresh PG databases and cleared, disposable side stores."""
import argparse
import base64
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import urllib.request
from urllib.parse import urlparse, urlunparse
import uuid
import importlib.util

ROOT = Path(__file__).resolve().parents[1]


def command(args, **kwargs):
    return subprocess.run(args, check=True, text=True, **kwargs)


def run():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--services", action="store_true", help="use explicitly opted-in CI services")
    parser.add_argument("--match", default="", help="run matching executable target names")
    parser.add_argument("--list", action="store_true")
    parser.add_argument("--shard", default="0/1", help="independent partition INDEX/COUNT; each run owns separate side stores")
    parser.add_argument("--timeout-secs", type=int, default=1800, help="per-suite deadline, including large CH/statistics suites")
    options = parser.parse_args()
    if options.timeout_secs <= 0:
        parser.error("--timeout-secs must be positive")
    try:
        shard_index, shard_count = map(int, options.shard.split("/"))
    except ValueError:
        parser.error("--shard must be INDEX/COUNT")
    if not 0 <= shard_index < shard_count <= 16:
        parser.error("--shard requires 0 <= INDEX < COUNT <= 16")
    tag = uuid.uuid4().hex[:12]
    owned = {}
    environment = os.environ.copy()
    environment["SQLX_OFFLINE"] = "true"
    environment["OKAPI_TEST_ISOLATED"] = "1"
    failures = []
    try:
        if options.services:
            if os.environ.get("OKAPI_TEST_ISOLATED") != "1":
                raise RuntimeError("--services requires OKAPI_TEST_ISOLATED=1")
            if not urlparse(environment["DATABASE_URL"]).path.startswith("/okapi_test_"):
                raise RuntimeError("refusing a non-test PostgreSQL database")
            redis_db = urlparse(environment["OKAPI_REDIS_URL"]).path.removeprefix("/")
            if not redis_db.isdecimal() or not 0 < int(redis_db) < 16:
                raise RuntimeError("refusing to flush Redis outside an explicit nonzero test database")
            if not shutil.which("psql") or not shutil.which("redis-cli"):
                raise RuntimeError("--services requires psql and redis-cli")
        else:
            for kind, image, container_port, extra in [
                ("pg", "postgres:16-alpine", 5432, ["-e", "POSTGRES_PASSWORD=okapi-test-local"]),
                ("redis", "redis:7-alpine", 6379, []),
                ("ch", "clickhouse/clickhouse-server:24.8-alpine", 8123, ["-e", "CLICKHOUSE_SKIP_USER_SETUP=1"]),
                ("nats", "nats:2-alpine", 4222, []),
            ]:
                name = f"okapi-test-{tag}-{kind}"
                owned[kind] = name
                args = ["docker", "run", "--rm", "-d", "--name", name, "-p", f"127.0.0.1::{container_port}", *extra, image]
                if kind == "nats": args.append("-js")
                command(args, stdout=subprocess.DEVNULL)
                port = command(["docker", "port", name, str(container_port)], capture_output=True).stdout.strip().rsplit(":", 1)[1]
                key, value = {
                    "pg": ("DATABASE_URL", f"postgres://postgres:okapi-test-local@127.0.0.1:{port}/okapi_test_base"),
                    "redis": ("OKAPI_REDIS_URL", f"redis://127.0.0.1:{port}/3"),
                    "ch": ("OKAPI_CLICKHOUSE_URL", f"http://127.0.0.1:{port}"),
                    "nats": ("OKAPI_NATS_URL", f"nats://127.0.0.1:{port}"),
                }[kind]
                environment[key] = value
            environment["OKAPI_MASTER_KEY"] = "0" * 64
        def ch_request(path="/", data=None):
            url = urlparse(environment["OKAPI_CLICKHOUSE_URL"])
            address = urlunparse(url._replace(netloc=f"{url.hostname}:{url.port or 8123}", path=path))
            request = urllib.request.Request(address, data=data)
            if url.username:
                credentials = f"{url.username}:{url.password or ''}".encode()
                request.add_header("Authorization", "Basic " + base64.b64encode(credentials).decode())
            return urllib.request.urlopen(request, timeout=30).read()

        environment.setdefault("OKAPI_MASTER_KEY", "0" * 64)
        environment.pop("OKAPI_TEST_CH_DATABASE", None)
        environment.pop("OKAPI_SETTLEMENT_JOURNAL_TAG", None)
        admin = urlparse(environment["DATABASE_URL"])
        admin_url = urlunparse(admin._replace(path="/postgres", query=""))

        def sql(statement, readiness=False):
            args = (["docker", "exec", owned["pg"], "psql", "-U", "postgres", "-d", "postgres"]
                    if owned else ["psql", admin_url])
            command([*args, "-v", "ON_ERROR_STOP=1", "-q", "-c", statement], stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL if readiness else None)

        for attempt in range(30):
            try:
                sql("SELECT 1", readiness=True)
                ch_request("/ping")
                break
            except Exception:
                if attempt == 29: raise
                time.sleep(0.5)
        build = subprocess.Popen(["cargo", "test", "--workspace", "--locked", "--no-run", "--message-format=json"],
                                 cwd=ROOT, env=environment, stdout=subprocess.PIPE, text=True)
        suites = {}
        for line in build.stdout:
            try: item = json.loads(line)
            except json.JSONDecodeError:
                print(line, end="", flush=True)
                continue
            if item.get("reason") == "compiler-artifact" and item.get("executable") and item["profile"]["test"]:
                suites[item["executable"]] = item["target"]["name"]
            if item.get("reason") == "compiler-message":
                print(item["message"].get("rendered", ""), end="", flush=True)
        if build.wait(): return 1
        spec = importlib.util.spec_from_file_location("nats_reset", ROOT / "scripts/nats-reset-stream.py")
        reset = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(reset)
        for index, (executable, target) in enumerate(sorted(suites.items())):
            if index % shard_count != shard_index: continue
            if options.match and options.match not in target: continue
            if options.list:
                print(target)
                continue
            database = f"okapi_test_{tag}_{index}"
            sql(f'CREATE DATABASE "{database}"')
            suite_env = environment.copy()
            suite_env["DATABASE_URL"] = urlunparse(admin._replace(path="/" + database))
            redis = (["docker", "exec", owned["redis"], "redis-cli", "-n", "3"] if owned else
                     ["redis-cli", "-u", environment["OKAPI_REDIS_URL"]])
            command([*redis, "FLUSHDB"], stdout=subprocess.DEVNULL)
            ch_request(data=b"DROP DATABASE IF EXISTS okapi")
            reset.reset(environment["OKAPI_NATS_URL"])
            print(f"\nRunning {target} ({executable})", flush=True)
            try:
                result = subprocess.run([executable, "--test-threads=1", "--nocapture"], cwd=ROOT, env=suite_env, timeout=options.timeout_secs)
                if result.returncode: failures.append(target)
            except subprocess.TimeoutExpired:
                print(f"Suite timed out after {options.timeout_secs}s: {target}", flush=True)
                failures.append(target + " (timeout)")
            finally:
                sql(f'DROP DATABASE "{database}" WITH (FORCE)')
        if not options.match and not options.list and shard_index == 0:
            result = subprocess.run(["cargo", "test", "--workspace", "--locked", "--doc"], cwd=ROOT, env=environment)
            if result.returncode: failures.append("doctests")
        print(f"Isolated shard {shard_index}/{shard_count} suite failures:", failures, flush=True)
        return int(bool(failures))
    finally:
        for name in owned.values():
            subprocess.run(["docker", "rm", "-f", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


if __name__ == "__main__":
    sys.exit(run())
