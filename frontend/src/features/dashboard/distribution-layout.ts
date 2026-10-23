/// 「用量分布」三个页签（模型 / 渠道 / Token）共用的版式。
///
/// 圆和图例在三个页签里的位置、大小必须一致，所以版面不能由各页签自己的内容撑出来：
/// - 页签头一行固定高度（lg:h-7）；
/// - 图区高度 = 面板高度 − 页签头 − 底部预留，与各页签底部放什么无关
///   （模型 / 渠道是汇总条，Token 是缓存上报条和来源详情）；
/// - 底部内容贴着面板底边：放得下就在预留里，放不下时面板在卡片内滚动，图区不动。
/// 大屏（lg）面板高度由首页网格给定，面板本身是尺寸容器（图区、圆的 cqh / cqw 以它们的容器为准）；
/// 窄屏自然高度，圆在上、图例在下。
///
/// 类名必须写成字面量（Tailwind 按源码文本扫描，不能拼接）。图区高度里的 `7.75rem`
/// = 页签头 `1.75rem` + 底部预留 `6rem`（含图区与底部的间距、面板下内边距），改一处要一起改。

/// 面板根：大屏是尺寸容器。
export const PANEL = 'flex h-full min-w-0 flex-col lg:[container-type:size]'
/// 页签头与其中的一行（描述文字在左，控件在右）。
export const HEAD = 'shrink-0 gap-0 px-5 pt-1 pb-2 lg:h-7 lg:py-0'
export const HEAD_ROW = 'flex min-w-0 flex-wrap items-center justify-between gap-x-2 text-xs text-muted-foreground lg:h-full lg:flex-nowrap'
/// 页签头里的「查看全部」类链接。
export const HEAD_LINK = 'inline-flex min-h-8 shrink-0 items-center gap-1 rounded text-xs text-primary outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40 lg:min-h-6'
/// 页签头下面的内容区。
export const BODY = 'flex flex-1 flex-col gap-3 px-5 pt-0 pb-3 lg:min-h-0 lg:gap-1.5 lg:px-4 lg:pb-2 lg:[@media(min-height:900px)]:px-5'
/// 图区：圆 + 图例，整体居中。大屏高度固定，自己也是尺寸容器。
export const STAGE = '@container flex flex-col items-center justify-center gap-4 @md:flex-row @md:gap-6 lg:h-[max(6rem,calc(100cqh-7.75rem))] lg:shrink-0 lg:flex-row lg:gap-5 lg:[container-type:size]'
/// 圆：取图区高 / 宽里较小的一个（宽取 42%，给图例留位置），最大 22rem。
export const PIE = 'size-40 shrink-0 lg:size-[clamp(5rem,min(100cqh,42cqw),22rem)]'
/// 图例列：右侧垂直居中，放不下时内部滚动。
export const LEGEND = 'flex w-full min-w-0 flex-col gap-1 @md:w-auto @md:max-w-[21rem] @md:flex-1 lg:max-h-full lg:w-auto lg:max-w-[21rem] lg:flex-1 lg:overflow-y-auto'
/// 底部：贴着面板底边。
export const FOOT = 'mt-auto flex flex-col gap-3 lg:gap-1.5'
