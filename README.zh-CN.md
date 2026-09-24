# PromptDock

自托管的 Codex 桌面会话与微信之间的通知桥。

PromptDock 通过现有的 Codex Hooks 采集桌面会话的 turn 边界，经 Relay 服务
投递结构化通知，并在你自己的域名下托管只读结果页。PromptDock 不执行任务、
不批准权限、不拦截或解密任何流量。

## 仓库组成

| 模块 | 路径 | 作用 |
|---|---|---|
| 桌面宿主 | `apps/desktop/` | Tauri 2 Windows 应用：与 Codex 桌面并行运行，持有本地运行收件箱、托盘、临时暂停投递、诊断面板 |
| Relay 服务 | `apps/server/` | Axum HTTP 服务：接收桌面 Hook 事件，扇出到微信，托管只读结果页 |
| 管理台 | `apps/admin/` | Vue 3 SPA：面向运维的设备 / 授权范围 / 投递 / 保留视图 |
| 契约 | `contracts/` | 冻结的 HTTP `/v1`、admin `/v2`、node-link `/v5` 三张线协议，附 SHA-256 manifest |
| 服务端 crates | `crates/relay-*` | 领域 / 应用 / 存储 (SQLite) / 传输 (HTTP + gateway) / 微信 Provider |
| 第三方 | `crates/wechat-ilink` | 派生自腾讯 `openclaw-weixin` v2.4.6 (MIT) |
| 部署 | `deploy/` | systemd unit、Caddyfile 示例、Docker Compose、配置示例 |

## 快速开始

本仓库使用 pnpm workspace（Node ≥ 24.18）和 Cargo workspace（Rust ≥ 1.98 stable）。
以下命令均在仓库根目录执行。

```
pnpm install
pnpm build          # 类型检查并构建 desktop 与 admin 的 Vue 前端
pnpm test           # 单元测试 + 契约测试
cargo build         # 构建服务端可执行（target: promptdock-relay）
```

三个构建面对应三条命令：`pnpm build` 只产出 Web 静态资源，不会生成
`PromptDockDesktop.exe`。Windows 桌面可执行文件由 Tauri CLI 构建，需要 Rust
工具链和 WebView2：

```
pnpm tauri:dev      # 桌面宿主热更新，Windows
pnpm tauri:build    # 生成 PromptDockDesktop.exe，Windows
```

完整的本地跑通见 `docs/getting-started/`；Linux 服务器与 Docker 部署见
`docs/self-hosting/`。

## 信任边界

- Codex Hook 事件只做观测，不做拦截。PromptDock 只读你在桌面装好 Hook
  后 Codex 自己写到 stdin / stdout 的内容。
- 结果页内容寻址、Token 门控、单一用途。FullFinal 输出一次一个链接（
  `result_pages_v1`），不再有分段包回退。
- 本地采集存储使用 Windows DPAPI（桌面）或服务端主密钥（Relay）加密。
  重置工具只清空固定白名单，保留 `capture-policy.json`。
- Relay 永远看不到桌面的私有工作目录、凭据，以及策略未明确启用的 Hook 输入。

在部署到公网之前请先读 `SECURITY.md`。

## 许可

本项目遵循 Apache License 2.0（以下简称"本许可证"）授权，协议全文见
[`LICENSE`](LICENSE)。本软件按"原样"提供，不附带任何明示或默示的担保，
详见本许可证第 7 条。随附第三方代码的授权与署名要求见
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)。

## 参与贡献

[`ARCHITECTURE.md`](ARCHITECTURE.md) 说明各组件分别负责哪一类决策，
[`AGENTS.md`](AGENTS.md) 列出改动需要保持的不变量，以及改动对应范围要跑的命令。
`CONTRIBUTING.md` 说明本地如何跑质量门、契约漂移如何被检测、如何提 issue。
开 PR 之前请先读 `docs/development/`。

## 状态

本 monorepo 由内部项目抽取而来，部分链路仍在源环境中才会端到端跑通，
因此公共测试覆盖单元与契约表面，但并非所有生产验收路径。实际发布内容
见 `CHANGELOG.md`。
