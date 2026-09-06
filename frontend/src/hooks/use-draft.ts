import { useCallback, useState } from 'react'

/// 过滤输入框的草稿值：用户敲字改草稿，回车 / 点搜索才把它提交进地址；
/// 反过来地址变了（后退、深链、清空筛选）输入框要跟着显示新值。
///
/// 对齐在渲染期做（React 会立刻带新状态重跑本次渲染），不走 useEffect：
/// 效果里对齐会先按旧草稿画一帧再跳到新值。
export function useDraft(applied: string): [string, (next: string) => void] {
  const [state, setState] = useState({ applied, draft: applied })
  if (state.applied !== applied) setState({ applied, draft: applied })
  const draft = state.applied === applied ? state.draft : applied
  const setDraft = useCallback((next: string) => setState((s) => ({ ...s, draft: next })), [])
  return [draft, setDraft]
}
