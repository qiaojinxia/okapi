/// 三视图（趋势 / 拆分 / 流向）共用的工作区卡片顶栏：
/// 左侧页签贴齐底线，右侧放置该视图的实时摘要，切换视图时顶栏高度与基线零抖动。
export function WorkspaceHeader({
  tabs,
  summary,
}: {
  tabs?: React.ReactNode
  summary?: React.ReactNode
}) {
  if (!tabs && !summary) return null
  return (
    <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 border-b border-border/75 bg-muted/15 px-4 pt-1 sm:px-5">
      <div className="-mb-px flex min-w-0 items-center">{tabs}</div>
      {summary && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 pb-1 text-xs text-muted-foreground tabular-nums">
          {summary}
        </div>
      )}
    </div>
  )
}
