import { useEffect } from 'react'

const FOCUSABLE = [
  'a[href]',
  'button:not([disabled])',
  'input:not([type=hidden]):not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',')

/// 当前活跃的模态层栈（后进先出）；只有栈顶拦截 Tab。
const stack: React.RefObject<HTMLElement | null>[] = []

function focusable(root: HTMLElement): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
    (el) => el.offsetParent !== null || el === document.activeElement,
  )
}

/// 模态层（抽屉 / 确认框）的焦点收口。
///
/// 为什么需要：这两个组件此前只做了"打开时把焦点送进去"，但焦点进去之后并不被关住——
/// 一路 Tab 会走到抽屉背后的表格与侧栏上去，屏幕阅读器读的是被 `aria-modal` 声明为
/// 不存在的内容，键盘用户则完全看不出焦点跑哪去了。关闭后焦点又落回 `<body>`，
/// 刚才点"编辑"的那一行位置就此丢失，要重新 Tab 几十次才回得去。
///
/// 三件事：进场把焦点送进 `initial`（缺省第一个可聚焦元素）、Tab / Shift+Tab 在层内循环、
/// 退场把焦点还给打开它的那个元素。
///
/// 层叠：确认框常常开在抽屉之上，此时抽屉的 `open` 仍是 true。只让**栈顶**那一层拦 Tab，
/// 否则两层各拦各的，焦点会被下层抽屉拽回去，确认框里根本走不动。
export function useModalFocus(
  open: boolean,
  panel: React.RefObject<HTMLElement | null>,
  /// 进场首选焦点：抽屉给第一个输入框，确认框给"确认"按钮。缺省取第一个可聚焦元素。
  initial?: (root: HTMLElement) => HTMLElement | null | undefined,
) {
  useEffect(() => {
    if (!open) return undefined
    // 记住开启者：可能是列表里的某个按钮，关闭后要还回去
    const opener = document.activeElement as HTMLElement | null
    stack.push(panel)

    // 首帧内容可能还没挂完（抽屉里的表单是同帧渲染的），下一个宏任务再取焦点
    const timer = window.setTimeout(() => {
      const root = panel.current
      if (!root) return
      const first = initial?.(root) ?? focusable(root)[0]
      first?.focus({ preventScroll: true })
    }, 30)

    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Tab') return
      if (stack[stack.length - 1] !== panel) return
      const root = panel.current
      if (!root) return
      const items = focusable(root)
      if (items.length === 0) return
      const first = items[0]
      const last = items[items.length - 1]
      const active = document.activeElement as HTMLElement | null
      // 焦点已在层外（浏览器地址栏回来、或上一轮被别处抢走）→ 拉回边界
      if (active === null || !root.contains(active)) {
        e.preventDefault()
        ;(e.shiftKey ? last : first).focus({ preventScroll: true })
        return
      }
      if (e.shiftKey && active === first) {
        e.preventDefault()
        last.focus({ preventScroll: true })
      } else if (!e.shiftKey && active === last) {
        e.preventDefault()
        first.focus({ preventScroll: true })
      }
    }
    document.addEventListener('keydown', onKey)

    return () => {
      window.clearTimeout(timer)
      document.removeEventListener('keydown', onKey)
      const at = stack.lastIndexOf(panel)
      if (at >= 0) stack.splice(at, 1)
      // 还焦点：元素可能已随列表刷新被卸载，isConnected 兜一下
      if (opener?.isConnected === true) opener.focus({ preventScroll: true })
    }
  }, [open, panel, initial])
}
