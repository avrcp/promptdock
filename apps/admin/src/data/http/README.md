# HTTP Admin Repository

`src/data/http/` 是 Admin API v2 的唯一浏览器 transport adapter。它不提供通用 Relay SDK，也不允许 feature 组件直接调用 `fetch`。

- `http-client.ts` 只构造 relative same-origin `/admin/api/v2` 请求，使用 `credentials: same-origin`、`cache: no-store`、purpose deadline、abort 和响应体上限。
- `@promptdock/relay-admin-api-generated` 以 strict Zod 校验每个服务端 success payload；未知字段、closed-enum drift 或 shape drift 都必须拒绝，不能在 mapper 中猜测兼容。
- read/command repository 通过 `AdminCapabilityMetadata` 在 mutation 前进行 capability preflight；服务端仍是唯一授权边界。
- mapper 只将验证过的 wire model 投影为 UI view model；不读取 SQLite、日志、文件、token 持久化或原始 provider payload。
- 成功 payload 的 schema 违规归一化为 `ADMIN_CONTRACT_MISMATCH`。当前 `list_runs` 不是 Admin API v2 的合法入站命令值，稳定值为 `list_jobs`。

合同的唯一 canonical 位于仓库根 `contracts/admin-api/v2/`。Rust producer 与 TypeScript consumer 在同一提交中共同验证 manifest、fixtures、routes、capabilities、actions 和 states。
