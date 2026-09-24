# PromptDock Relay Admin

PromptDock Relay Admin 是仓库内的 Relay Operator Console。它与 Relay Server 共用同一个
workspace 版本、Admin API v2 合同（`contracts/admin-api/v2/`）与同一个发布 bundle；不
再拥有独立的发布或部署入口。

浏览器只通过同源 `/admin/api/v2` 管理 Relay。Admin 不读取本地 SQLite、Shell 或文件，
也不提供 Harness、模型或 Agent 管理界面。生产流量由 Caddy 在同一 origin 下将静态 UI
和 API 分别路由到 Admin 产物与 Relay 的 loopback-only 管理监听器，因此不需要 CORS。

## 开发与验证

所有命令都从仓库根目录运行。Admin 的脚本通过 pnpm filter 调用：

```bash
corepack pnpm@11.10.0 install --frozen-lockfile
corepack pnpm@11.10.0 --filter @promptdock/relay-admin dev
corepack pnpm@11.10.0 --filter @promptdock/relay-admin quality
corepack pnpm@11.10.0 --filter @promptdock/relay-admin test
corepack pnpm@11.10.0 --filter @promptdock/relay-admin build:production
```

根目录 `scripts/quality/quality.sh` 会依次运行合同漂移检查、Rust workspace 门、Admin 与
Desktop 的 ESLint、TypeScript、Vitest、设计令牌门和生产构建。`test:e2e:integration` 需要
一个可访问的 Relay 实例，只在显式触发时运行。

## 发布边界

发布 bundle 是一个目录，包含 Relay binary、`admin/` 静态产物、`contracts/`、部署资产与
校验 manifest。安装、升级和回滚必须以整个 bundle 为单位，禁止单独复制 Admin dist 或形成
第二套版本号。`scripts/server/lib/validate-release-bundle.py` 是该布局的权威校验器。

更多信息见：

- [架构与合同](../../docs/architecture/contracts.md)
- [Linux 自托管](../../docs/self-hosting/linux.md)
- [Docker 自托管](../../docs/self-hosting/docker.md)
- [开发指南](../../docs/development/README.md)
