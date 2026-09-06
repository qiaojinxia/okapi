#!/usr/bin/env python3
"""校验 deploy/ 下的 compose 与 K8s 模板：结构完整、镜像来源、探针、下线宽限期、连接数总账。

为什么要有这道闸：这些模板不在任何编译期或测试期路径上，改坏了只会在发布那天才知道。
本机没有 compose 插件与 kubectl，所以只做与部署语义强相关的结构断言，而不是 schema 校验：
- 每个 K8s 文档都有 apiVersion / kind / metadata.name；Service 的 selector 必须对得上某个 Deployment；
- 每个容器有 image、resources.requests / limits；对外服务的容器有 /healthz readinessProbe；
- gateway 的 terminationGracePeriodSeconds 与 compose 应用服务的 stop_grace_period 必须覆盖
  IMPLEMENTATION §14.3 的排水口径（SSE 5min + 后台结算 30s = 330s）——进程现在真的会等，
  编排层宽限期不够就等于把优雅下线又改回硬杀；
- K8s 的 Σ(副本上限 × OKAPI_PG_POOL) 不得超过模板注释里承诺的 PG max_connections 预算；
- compose 依赖镜像来自 public.ecr.aws（项目镜像拉取约定），clickhouse 官方非 library 镜像除外。

YAML 解析优先用 PyYAML；缺失时借系统 ruby 的 Psych 转成 JSON（macOS 自带）。
"""

import json
import re
import shutil
import subprocess
import sys

K8S = 'deploy/k8s/okapi.yaml'
COMPOSE = 'deploy/docker-compose.yml'
COMPOSE_DEV = 'deploy/docker-compose.dev.yml'
DRAIN_SECS = 330
PG_BUDGET = 200
IMAGE_PREFIXES = ('public.ecr.aws/', 'clickhouse/')


def load_yaml_docs(path: str) -> list:
    try:
        import yaml  # type: ignore

        with open(path, encoding='utf-8') as f:
            return [d for d in yaml.safe_load_all(f) if d is not None]
    except ModuleNotFoundError:
        pass
    if shutil.which('ruby') is None:
        raise SystemExit('❌ 既无 PyYAML 也无 ruby，无法解析 YAML（pip install pyyaml）')
    out = subprocess.run(
        ['ruby', '-ryaml', '-rjson', '-e',
         'puts YAML.load_stream(File.read(ARGV[0]), aliases: true).compact.to_json rescue '
         'puts YAML.load_stream(File.read(ARGV[0])).compact.to_json', path],
        capture_output=True, text=True, check=True,
    )
    return json.loads(out.stdout)


def duration_secs(raw) -> int:
    """compose 时长（"5m30s" / "90s" / "1h"）→ 秒。"""
    if isinstance(raw, (int, float)):
        return int(raw)
    total = 0
    for value, unit in re.findall(r'(\d+)\s*(h|m|s)', str(raw)):
        total += int(value) * {'h': 3600, 'm': 60, 's': 1}[unit]
    return total


def check_k8s(problems: list[str]) -> str:
    docs = load_yaml_docs(K8S)
    deployments = {}
    for d in docs:
        for key in ('apiVersion', 'kind'):
            if key not in d:
                problems.append(f'{K8S}: 文档缺 {key}')
        name = d.get('metadata', {}).get('name')
        if not name:
            problems.append(f'{K8S}: {d.get("kind")} 缺 metadata.name')
        if d.get('kind') == 'Deployment':
            deployments[name] = d
    pool_total = 0
    hpa_max = {d['spec']['scaleTargetRef']['name']: d['spec']['maxReplicas']
               for d in docs if d.get('kind') == 'HorizontalPodAutoscaler'}
    for target in hpa_max:
        if target not in deployments:
            problems.append(f'{K8S}: HPA 指向不存在的 Deployment {target}')
    for name, dep in deployments.items():
        spec = dep['spec']['template']['spec']
        labels = dep['spec']['template']['metadata']['labels']
        if dep['spec']['selector']['matchLabels'] != labels:
            problems.append(f'{K8S}: {name} selector 与 pod labels 不一致')
        replicas = hpa_max.get(name, dep['spec'].get('replicas', 1))
        for c in spec.get('containers', []):
            if 'image' not in c:
                problems.append(f'{K8S}: {name}/{c.get("name")} 缺 image')
            res = c.get('resources', {})
            if 'requests' not in res or 'limits' not in res:
                problems.append(f'{K8S}: {name}/{c.get("name")} 缺 resources.requests/limits')
            env = {e['name']: e.get('value') for e in c.get('env', []) if 'name' in e}
            pool_total += replicas * int(env.get('OKAPI_PG_POOL', '16'))
            if c.get('ports'):
                probe = c.get('readinessProbe', {}).get('httpGet', {})
                if probe.get('path') != '/healthz':
                    problems.append(f'{K8S}: {name}/{c.get("name")} 对外服务缺 /healthz readinessProbe')
        if labels.get('role') == 'gateway':
            grace = spec.get('terminationGracePeriodSeconds', 30)
            if grace < DRAIN_SECS:
                problems.append(
                    f'{K8S}: {name} terminationGracePeriodSeconds={grace} < {DRAIN_SECS}（§14.3 排水口径）')
    for d in docs:
        if d.get('kind') == 'Service':
            sel = d['spec'].get('selector', {})
            if not any(dep['spec']['template']['metadata']['labels'] == sel for dep in deployments.values()):
                problems.append(f'{K8S}: Service {d["metadata"]["name"]} 的 selector 没有对应 Deployment')
    if pool_total > PG_BUDGET:
        problems.append(f'{K8S}: Σ(副本上限 × OKAPI_PG_POOL) = {pool_total} > 预算 {PG_BUDGET}（改 HPA 时同步改池大小）')
    return f'{len(deployments)} 个 Deployment，PG 连接总账 {pool_total}/{PG_BUDGET}'


def check_compose(path: str, problems: list[str], app_services: bool) -> str:
    (doc,) = load_yaml_docs(path)
    services = doc.get('services', {})
    if not services:
        problems.append(f'{path}: 没有 services')
    apps = 0
    for name, svc in services.items():
        image = svc.get('image')
        if image is not None and not image.startswith(IMAGE_PREFIXES):
            problems.append(f'{path}: {name} 镜像 {image} 不在允许的来源（{", ".join(IMAGE_PREFIXES)}）')
        if image is None and 'build' not in svc:
            problems.append(f'{path}: {name} 既无 image 也无 build')
        if 'build' in svc:
            apps += 1
            grace = duration_secs(svc.get('stop_grace_period', '10s'))
            if grace < DRAIN_SECS:
                problems.append(f'{path}: {name} stop_grace_period={grace}s < {DRAIN_SECS}s（§14.3 排水口径）')
            deps = svc.get('depends_on', {})
            if isinstance(deps, dict):
                for dep, cond in deps.items():
                    if cond.get('condition') != 'service_healthy':
                        problems.append(f'{path}: {name} 依赖 {dep} 未等 service_healthy')
                    if 'healthcheck' not in services.get(dep, {}):
                        problems.append(f'{path}: {dep} 被 service_healthy 依赖却没有 healthcheck')
    if app_services and apps == 0:
        problems.append(f'{path}: 没有任何应用服务（build 段）')
    return f'{len(services)} 个服务，{apps} 个应用服务'


def main() -> int:
    problems: list[str] = []
    k8s = check_k8s(problems)
    compose = check_compose(COMPOSE, problems, app_services=True)
    dev = check_compose(COMPOSE_DEV, problems, app_services=False)
    if problems:
        print(f'❌ guard-deploy-manifests：{len(problems)} 处问题')
        for p in problems:
            print(f'   {p}')
        return 1
    print(f'✅ guard-deploy-manifests：k8s {k8s}；compose {compose}；compose.dev {dev}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
