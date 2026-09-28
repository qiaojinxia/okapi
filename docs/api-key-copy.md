# 密钥列表复制

`0026_api_key_ciphertext.sql` 为 API key 增加可空加密副本。认证仍使用 SHA-256；网关认证路径不解密。历史 key 不补写、不轮换、不吊销，哈希不可逆。

`POST /auth/keys` 在配置 `OKAPI_MASTER_KEY` 时使用 AES-256-GCM 保存完整 Token；信封以 `okk1` 标识，随机 nonce，AAD 绑定用户 ID 与认证哈希，防止跨用户/跨行交换。未配置主密钥仍可创建 hash-only key，响应 `copy_available=false`，不会写明文。配置无效时拒绝创建，不降级。

`GET /api/me/keys` 只增加 `copy_status` 元数据（`available` / `not_saved` / `unavailable`），不包含明文或密文。列表点击复制后调用 `POST /auth/keys/{id}/copy`，发送 JSON `{}`，要求有效 web session 与同一用户的 Bearer 身份；仅 API Key 登录、跨用户和已删除 key 均不能读取。启停/过期不改变加密副本，只影响调用权限。响应标记 `Cache-Control: no-store, private`；复制默认限流 30 次/IP/分钟。

前端明文仅用于当前点击的剪贴板写入，不加入查询缓存、持久化存储或列表 DOM。未保存加密副本的 key 提示重新创建，但不自动替换现有 key。创建成功面板保留即时复制，按 `copy_available` 显示不同保存提示。

部署前备份数据库，升级迁移并重启后端。妥善备份主密钥；任意替换它会使已保存的副本无法解密，但不影响原 Token 的哈希认证。需要轮换主密钥时，应在受控流程中使用旧密钥解密再以新密钥加密，不能直接覆盖环境变量。
