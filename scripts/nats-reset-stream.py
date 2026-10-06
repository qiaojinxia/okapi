#!/usr/bin/env python3
"""Delete the development BILLING stream and consumer state through the NATS API."""
import json
import os
import socket
import uuid
from urllib.parse import urlparse, unquote


def reset(url):
    parsed = urlparse(url)
    if parsed.scheme != "nats":
        raise ValueError("development reset expects a nats:// URL")
    credentials = {"verbose": False, "pedantic": True}
    if parsed.password:
        credentials.update(user=unquote(parsed.username or ""), **{"pass": unquote(parsed.password)})
    elif parsed.username:
        credentials["auth_token"] = unquote(parsed.username)
    with socket.create_connection((parsed.hostname, parsed.port or 4222), timeout=5) as connection:
        stream = connection.makefile("rb")
        if not stream.readline().startswith(b"INFO "):
            raise RuntimeError("invalid NATS greeting")
        inbox = "_INBOX.okapi_reset_" + uuid.uuid4().hex
        wire = (f"CONNECT {json.dumps(credentials)}\r\nSUB {inbox} 1\r\n"
                f"PUB $JS.API.STREAM.DELETE.BILLING {inbox} 2\r\n{{}}\r\nPING\r\n")
        connection.sendall(wire.encode())
        for _ in range(20):
            line = stream.readline()
            if line.startswith(b"MSG "):
                size = int(line.split()[-1])
                response = json.loads(stream.read(size))
                stream.read(2)
                error = response.get("error")
                if error and error.get("code") != 404:
                    raise RuntimeError(f"NATS reset failed: {error}")
                return
            if line.startswith(b"-ERR") or not line:
                raise RuntimeError("NATS reset refused")
            if line == b"PING\r\n":
                connection.sendall(b"PONG\r\n")
        raise RuntimeError("missing NATS reset reply")


if __name__ == "__main__":
    reset(os.environ["OKAPI_NATS_URL"])
