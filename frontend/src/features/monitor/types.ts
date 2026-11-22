export interface Probe {
  ok: boolean
  /// 未配置（ClickHouse / NATS 可选）。
  configured?: boolean
  error?: string
}

export interface TableSize {
  name: string
  bytes: number | null
  rows: number | null
  parts?: number | null
}

export interface PostgresProbe extends Probe {
  version?: string
  uptime_secs?: number
  connections?: number
  active?: number
  idle_in_transaction?: number
  max_connections?: number
  longest_query_secs?: number | null
  database_bytes?: number
  cache_hit_ratio?: number | null
  deadlocks?: number
  pool?: { size: number; idle: number; max: number }
  tables?: TableSize[]
}

export interface RedisProbe extends Probe {
  version?: string
  uptime_secs?: number
  clients?: number
  blocked_clients?: number
  used_memory?: number
  used_memory_peak?: number
  used_memory_rss?: number
  maxmemory?: number | null
  maxmemory_policy?: string
  fragmentation_ratio?: number
  ops_per_sec?: number
  hit_ratio?: number | null
  evicted_keys?: number
  keys?: number
  rdb_last_bgsave_status?: string
}

export interface ClickhouseProbe extends Probe {
  version?: string
  uptime_secs?: number
  queries?: number
  tcp_connections?: number
  http_connections?: number
  background_merges?: number
  memory_resident?: number | null
  os_memory_total?: number | null
  disks?: { name: string; free_bytes: number | null; total_bytes: number | null }[]
  database_bytes?: number
  parts?: number
  tables?: TableSize[]
}

export interface NatsProbe extends Probe {
  state?: string
  version?: string
  in_bytes?: number
  out_bytes?: number
  in_messages?: number
  out_messages?: number
  reconnects?: number
  jetstream?: {
    memory_bytes?: number
    storage_bytes?: number
    streams?: number
    consumers?: number
    max_storage?: number | null
    error?: string
  }
  streams?: { name: string; messages: number; bytes: number; consumers: number }[]
}

export interface Host {
  cpus: number | null
  load: [number, number, number] | null
  memory: { total_bytes: number; available_bytes: number; swap_total_bytes: number; swap_free_bytes: number } | null
  disk: { total_bytes: number; free_bytes: number } | null
  uptime_secs: number | null
  process: { rss_bytes: number; threads: number; open_fds: number | null } | null
}

export interface Overview {
  collected_at: string
  node: string
  host: Host
  rates: { cpu_percent: number | null; net_rx_bps: number | null; net_tx_bps: number | null }
  postgres: PostgresProbe
  redis: RedisProbe
  clickhouse: ClickhouseProbe
  nats: NatsProbe
}

export interface Sample {
  t: number
  node: string
  cpu: number | null
  load1: number | null
  mem_used: number | null
  mem_total: number | null
  disk_used: number | null
  disk_total: number | null
  net_rx: number | null
  net_tx: number | null
  pg_conns: number | null
  pg_active: number | null
  redis_mem: number | null
  redis_clients: number | null
  redis_ops: number | null
  ch_mem: number | null
  ch_queries: number | null
  gw_rps?: number | null
  gw_5xx_rate?: number | null
  gw_p95_ms?: number | null
  ttft_p95_ms?: number | null
  failovers_pm?: number | null
  compactions_pm?: number | null
  settle_backlog?: number | null
}

export interface HistogramSummary {
  count: number
  avg: number | null
  p50: number | null
  p95: number | null
  p99: number | null
}

export interface CacheEntries {
  models: number
  candidates: number
  channel_cost: number
  channel_control: number
  pool_policies: number
  settings: number
}

export interface RuntimeMetrics {
  pg_pool_size: number
  pg_pool_idle: number
  pg_pool_active: number
  pricebook_epoch: number
  settle_backlog: number
  settle_in_flight: number
  settle_shedding: boolean
  caches: CacheEntries
}

export interface ClusterMetricsSummary {
  gateway_rps: number
  console_rps: number
  gateway_in_flight: number
  console_in_flight: number
  gateway_requests_total: number
  gateway_5xx_total: number
  gateway_5xx_rate: number | null
  auth_cache_hits: number
  auth_cache_misses: number
  auth_cache_hit_ratio: number | null
  upstream_failovers: number
  failovers_per_min: number
  settle_backlog: number
  settle_in_flight: number
  settle_shedding: boolean
  settle_batches_ok: number
  settle_batches_fallback: number
  settle_batch_size: HistogramSummary
  settle_batch_duration_ms: HistogramSummary
  cache_compactions: number
  compactions_per_min: number
  cache_compaction_saved_chars: number
  pool_quality_events_total: number
  routing_invalidations_local: number
  routing_invalidations_broadcast: number
  gateway_latency_ms: HistogramSummary
  upstream_ttft_ms: HistogramSummary
  recent_gw_p95_ms: number | null
  recent_ttft_p95_ms: number | null
}

export interface ProviderMetricsRow {
  provider: string
  total: number
  ok: number
  errors: number
  success_ratio: number | null
  outcomes: Record<string, number>
  ttft_ms: HistogramSummary
}

export interface HttpRouteMetricsRow {
  role: string
  route: string
  total: number
  s2xx: number
  s4xx: number
  s5xx: number
  latency_ms: HistogramSummary
}

export interface PoolQualityEventRow {
  pool: string
  action: string
  count: number
}

export interface WorkerJobRow {
  job: string
  total: number
  outcomes: Record<string, number>
}

export interface NodeMetricsSummary {
  node: string
  role: string
  ts: number
  gateway_rps: number
  console_rps: number
  in_flight: number
  runtime: RuntimeMetrics
}

export interface MetricsResp {
  collected_at: string
  source: 'shared' | 'local'
  node_count: number
  prometheus_path: string
  summary: ClusterMetricsSummary
  providers: ProviderMetricsRow[]
  routes: HttpRouteMetricsRow[]
  pool_quality_events: PoolQualityEventRow[]
  worker_jobs: WorkerJobRow[]
  nodes: NodeMetricsSummary[]
}

export interface LogEntry {
  ts: string
  level: 'WARN' | 'ERROR'
  target: string
  message: string
  node: string
  role: string
}

export interface LogsResp {
  source: 'shared' | 'local'
  total: number
  errors: number
  data: LogEntry[]
}
