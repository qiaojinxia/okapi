// 首屏前应用持久化的主题，避免亮→暗闪烁。外置成文件而不是内联：CSP 只放行同源脚本（script-src 'self'）。
// Apply the persisted theme before first paint to avoid a light->dark flash.
// Mirrors src/lib/theme.ts (storage key + system fallback).
;(function () {
  try {
    var pref = localStorage.getItem('okapi.theme')
    var dark =
      pref === 'dark' ||
      (pref !== 'light' && window.matchMedia('(prefers-color-scheme: dark)').matches)
    if (dark) document.documentElement.classList.add('dark')
  } catch {
    // 读不到存储（隐私模式等）就按浅色首屏，交给应用启动后再判
  }
})()
