#!/usr/bin/env python3
"""校验 deploy/ 下的 compose 与 K8s 模板：结构完整、镜像来源、探针、下线宽限期、连接数总账。

为什么要有这道闸：这些模板不在任何编译期或测试期路径上，改坏了只会在发布那天才知道。
本机没有 compose 插件与 kubectl，所以只做与部署语义强相关的结构断言，而不是 schema 校验：
- 每个 K8s 文档都有 apiVersion / kind / metadata.name；Service 的 selector 必须对得上某个 Deployment；
- 每个容器有 image、resources.requests / limits；对外服务的容器有 /healthz readinessProbe，有 livenessProbe 的还得有 startupProbe；
- gateway 与 console 的 terminationGracePeriodSeconds（console 也承载付费 SSE）与 compose 应用服务的 stop_grace_period 必须覆盖
  IMPLEMENTATION §14.3 的排水口径（SSE 5min + 后台结算 30s = 330s）——进程现在真的会等，
  编排层宽限期不够就等于把优雅下线又改回硬杀；
- K8s 的 Σ(副本上限 × OKAPI_PG_POOL) 不得超过模板注释里承诺的 PG max_connections 预算；
- K8s 的 OKAPI_TRUSTED_PROXIES 不得放行整个私网段（集群内任何 pod 都在里面，等于谁都能伪造来源 IP）；
- compose 依赖镜像来自 public.ecr.aws（项目镜像拉取约定），clickhouse 官方非 library 镜像除外；
- nginx 模板与网关路由表不能漂移：docs/api-surface.json 里网关注册的每个路径前缀都要被某个转给网关的 location
  接住（漏了就落进 `location /` 被转给控制面，客户端拿到 SPA 的 HTML 壳）；网关里最大的请求体上限
  （`DefaultBodyLimit::max`）不能比模板里转给网关的 location 的 `client_max_body_size` 更大（否则超过 nginx
  上限的请求在反代就 413 了）；访问日志格式不得写原始 `$request` / 查询串（Gemini 的 ?key= 会进日志）。

YAML 解析优先用 PyYAML；缺失时借系统 ruby 的 Psych 转成 JSON（macOS 自带）。
"""

import json
import re
import shutil
import subprocess
import sys

K8S = 'deploy/k8s/okapi.yaml'
NGINX = 'deploy/nginx-sse.conf'
COMPOSE = 'deploy/docker-compose.yml'
COMPOSE_DEV = 'deploy/docker-compose.dev.yml'
API_SURFACE = 'docs/api-surface.json'
GATEWAY_ROUTER = 'bins/okapi/src/gateway/mod.rs'
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
            broad = {'10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16'} & {
                e.strip() for e in (env.get('OKAPI_TRUSTED_PROXIES') or '').split(',')
            }
            if broad:
                problems.append(f'{K8S}: {name} 的 OKAPI_TRUSTED_PROXIES 放行整个私网段 {sorted(broad)}——'
                                '集群内任何 pod 都能伪造转发头；用 OKAPI_EDGE_KEY 或 Ingress 控制器网段')
            if c.get('ports'):
                probe = c.get('readinessProbe', {}).get('httpGet', {})
                if probe.get('path') != '/healthz':
                    problems.append(f'{K8S}: {name}/{c.get("name")} 对外服务缺 /healthz readinessProbe')
                # 有 liveness 就得有 startupProbe：否则首启慢迁移期间 kubelet 会反复杀容器
                if 'livenessProbe' in c and 'startupProbe' not in c:
                    problems.append(f'{K8S}: {name}/{c.get("name")} 有 livenessProbe 却没有 startupProbe（慢迁移会被反复杀）')
        if labels.get('role') in ('gateway', 'console'):
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


def check_nginx(problems: list[str]) -> str:
    """控制面里的两条 SSE（Playground 中继、渠道试用）不在 /v1/ 下，必须有自己的 location：
    关缓冲、放宽读超时；否则落在 location / 里被缓冲成整段、60s 静默就被切。
    另外核对网关路由前缀覆盖、请求体上限与访问日志脱敏（见下面的子检查）。"""
    with open(NGINX, encoding='utf-8') as f:
        text = f.read()
    block = re.search(r'location\s+~\s+[^{]*playground/chat[^{]*\{(.*?)\n\s*\}', text, re.S)
    if block is None:
        problems.append(f'{NGINX}: 缺少 /api/me/playground/chat 与 /admin/channels/*/try 的 SSE location')
        return 'SSE location 缺失'
    body = block.group(1)
    if not re.search(r'proxy_buffering\s+off\s*;', body):
        problems.append(f'{NGINX}: 控制面 SSE location 没有 proxy_buffering off')
    if not re.search(r'proxy_read_timeout\s+(\d+[smh]|\d+)\s*;', body):
        problems.append(f'{NGINX}: 控制面 SSE location 没有放宽 proxy_read_timeout')
    if 'admin/channels' not in block.group(0):
        problems.append(f'{NGINX}: 控制面 SSE location 没覆盖 /admin/channels/*/try')
    prefixes = check_nginx_gateway_routes(text, problems)
    check_nginx_access_log(text, problems)
    return f'控制面 SSE location 就位，网关 {prefixes} 个路径前缀全覆盖，访问日志脱敏'


def nginx_locations(text: str) -> list[tuple[str, str, str]]:
    """[(修饰符, 匹配串, location 块内容)]。"""
    found = []
    for m in re.finditer(r'location\s+(?:(=|~\*|~|\^~)\s+)?(\S+)\s*\{', text):
        depth, i = 1, m.end()
        while depth and i < len(text):
            depth += (text[i] == '{') - (text[i] == '}')
            i += 1
        found.append((m.group(1) or '', m.group(2), text[m.end():i - 1]))
    return found


def nginx_bytes(raw: str) -> int:
    value, unit = re.fullmatch(r'(\d+)([kKmMgG]?)', raw).groups()
    return int(value) * {'': 1, 'k': 1 << 10, 'm': 1 << 20, 'g': 1 << 30}[unit.lower()]


def eval_product(expr: str) -> int:
    """`128 * 1024 * 1024` 这种只含整数与乘号的 Rust 常量表达式。"""
    total = 1
    for part in expr.split('*'):
        total *= int(part.strip().replace('_', ''))
    return total


def check_nginx_gateway_routes(text: str, problems: list[str]) -> int:
    """网关路由表 ↔ 模板的 location：路径前缀要全被接住，请求体上限不能被反代抢先卡掉。"""
    with open(API_SURFACE, encoding='utf-8') as f:
        endpoints = json.load(f)['endpoints']
    # /healthz 控制面也有，反代不必单独转给网关
    prefixes = sorted({'/' + e['path'].strip('/').split('/')[0] for e in endpoints if e['surface'] == 'gateway'} - {'/healthz'})
    to_gateway = [(mod, pat, body) for mod, pat, body in nginx_locations(text) if 'okapi_gateway' in body]

    def covered(path: str) -> bool:
        for mod, pat, _ in to_gateway:
            if mod in ('', '^~') and path.startswith(pat):
                return True
            if mod in ('~', '~*') and re.search(pat, path, re.I if mod == '~*' else 0):
                return True
        return False

    for prefix in prefixes:
        if not covered(f'{prefix}/x'):
            problems.append(f'{NGINX}: 网关注册了 {prefix}/ 下的路由，模板里没有 location 把它转给 okapi_gateway'
                            '（会落进 location / 被转给控制面，客户端拿到 SPA 的 HTML 壳）')

    with open(GATEWAY_ROUTER, encoding='utf-8') as f:
        router = f.read()
    limits = [eval_product(m.group(1)) for m in re.finditer(r'DefaultBodyLimit::max\(\s*([0-9_ *]+?)\s*\)', router)]
    if limits:
        need = max(limits)
        have = [nginx_bytes(m.group(1)) for _, _, body in to_gateway
                for m in re.finditer(r'client_max_body_size\s+(\d+[kKmMgG]?)\s*;', body)]
        if not have or max(have) < need:
            problems.append(f'{NGINX}: 网关有一条路由的请求体上限是 {need} 字节，模板里没有转给网关的 location 放得下'
                            f'（最大 {max(have) if have else "未设"}）——超过 server 级 client_max_body_size 的请求在反代就 413 了')
    return len(prefixes)


def check_nginx_access_log(text: str, problems: list[str]) -> None:
    """访问日志格式不能写原始 $request / 查询串：Gemini 原生入口支持 ?key=<API key>。"""
    fmt = re.search(r'log_format\s+okapi_redacted\s+(.*?);', text, re.S)
    if fmt is None:
        problems.append(f'{NGINX}: 缺少脱敏的 log_format okapi_redacted（缺省 combined 会把 ?key= 写进访问日志）')
        return
    if re.search(r'\$request\b|\$request_uri|\$args|\$query_string|\$is_args', fmt.group(1)):
        problems.append(f'{NGINX}: log_format okapi_redacted 直接写了原始 $request / 查询串，?key= 会进日志')
    if not re.search(r'map\s+\$request\s+\$okapi_logged_request\s*\{[^}]*key=', text):
        problems.append(f'{NGINX}: 缺少把 key= 参数值抹掉的 map $request $okapi_logged_request')
    if not re.search(r'access_log\s+\S+\s+okapi_redacted\s*;', text):
        problems.append(f'{NGINX}: server 里没有用 okapi_redacted 记访问日志（access_log … okapi_redacted;）')


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
    nginx = check_nginx(problems)
    compose = check_compose(COMPOSE, problems, app_services=True)
    dev = check_compose(COMPOSE_DEV, problems, app_services=False)
    if problems:
        print(f'❌ guard-deploy-manifests：{len(problems)} 处问题')
        for p in problems:
            print(f'   {p}')
        return 1
    print(f'✅ guard-deploy-manifests：k8s {k8s}；nginx {nginx}；compose {compose}；compose.dev {dev}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
