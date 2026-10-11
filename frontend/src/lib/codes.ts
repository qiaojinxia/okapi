/// 机器标识（分组 / 池 / 套餐 / 出口组的 code）：与后端 console/identifiers.rs 的 ensure_code 同一规则。
/// 这些 code 会拼进 Redis 键与 `<group>|<channel_id>` 这类字段，`|`、`:`、空白都不能进来。
export function isMachineCode(value: string, max = 32): boolean {
  const code = value.trim()
  return code.length > 0 && code.length <= max && /^[A-Za-z0-9_.-]+$/.test(code)
}
