# LoopMe v0 架构与协议

产品决策见 [product-design.md](product-design.md)，构建步骤见 [build.md](build.md)。

## 组成

```
iPhone (LoopMe iOS) ──WebSocket + 端到端加密──┐
                                              ├── loopme-daemon (Rust, 跑在 Mac 上) ── codex app-server (子进程)
Mac 菜单栏 App ────── 同一协议, 127.0.0.1 ─────┘          │
                                                          ├── SQLite (~/Library/Application Support/LoopMe/loopme.db)
                                                          └── APNs (直接调用)
```

| 目录 | 内容 |
| --- | --- |
| `daemon/` | Rust daemon：存储、协议、配对、Codex 引擎、调度、通知 |
| `apple/LoopMeKit/` | Swift 包：模型、加密、连接、状态（`LoopMeStore`）、共享界面（对话、卡片、设置） |
| `apple/iOS/` | iOS App：扫码连接、对话、设置、推送 |
| `apple/Mac/` | 菜单栏 App：启动 daemon、二维码配对、Codex 状态、对话窗口 |
| `tools/` | 假 Codex、Python 参考客户端、端到端测试 |

## 与设计文档的差异（v0）

- **网络层**：设计定的是 iroh + 内网 relay。为了保证明天能直接构建联调，v0 先用“手机经 VPN/局域网直连 Mac 的 WebSocket（端口 7788）+ 应用层端到端加密”。加密方案和 iroh 无关，以后换 iroh 只需替换传输层，协议和配对不变。
- **daemon 托管**：v0 由菜单栏 App 以子进程方式启动 daemon（退出 App 即停止）。设计中的 `SMAppService.agent`（LaunchAgent）放到下一步。
- **共享核心**：设计是 Rust `loopme-core` 通过 UniFFI 给 iOS 用。v0 的 iOS 端用 CryptoKit 原生实现同一套加密（两边用同一组测试向量校验），省去 UniFFI 构建链。
- **Agent 输出结构化信息**：用回复末尾的 `<loopme>{json}</loopme>` 控制块（对用户隐藏），而不是 Codex 的实验性 dynamic tools。

## 加密与配对

- 身份密钥：X25519。Mac 一把，每台设备一把。
- 会话密钥：`HKDF-SHA256(ikm = X25519(设备私钥, Mac 公钥), salt = clientNonce(16) ‖ serverNonce(16))`，info 分别为 `loopme v1 c2s`、`loopme v1 s2c`。
- 帧：ChaCha20-Poly1305，线上格式即 CryptoKit 的 `SealedBox.combined`：`nonce(12) ‖ 密文 ‖ tag(16)`；nonce 为 4 个 0 字节加 64 位小端计数器，接收方拒绝不递增的计数器。
- 配对二维码：`loopme://pair?v=1&k=<Mac 公钥>&c=<一次性配对码>&h=<地址,地址>&p=<端口>&n=<Mac 名称>`，5 分钟有效，只能用一次。
- 配对流程（`/v1/pair`，明文 JSON）：手机发 `{"t":"pair","code","devicePub","deviceName"}` → Mac 回 `{"t":"pair_pending","sas"}` → 用户核对 4 位校验码 `SAS = SHA256("loopme v1 sas" ‖ Mac 公钥 ‖ 设备公钥)` 前 4 字节 mod 10000，在 Mac 上确认 → `{"t":"paired","deviceId"}`。Mac 公钥来自二维码，校验码防止中间人替换手机公钥。

## 会话协议（`/v1`）

1. 明文握手：手机 `{"t":"hello","deviceId","nonce"}`，Mac `{"t":"hello","nonce"}`；设备已解除配对时回 `{"t":"error","code":"unknown_device"}`。
2. 之后全部是加密的二进制帧，内容为 JSON：
   - 请求 `{"id","method","params"}`，响应 `{"id","result"}` 或 `{"id","error":{"code","message"}}`。
   - 事件 `{"event","seq","data"}`：`message`、`messageDelta`（流式全文）、`item`、`decision`、`memory`、`profile`、`settings`、`status`、`devices`、`pairing`、`resync`。

| 方法 | 说明 |
| --- | --- |
| `sync` | 全量快照（最近 300 条消息、事项、决定、记忆、Profile、设置、状态、设备） |
| `message.send {clientId, text}` | 发消息；`clientId` 去重 |
| `run.stop` | 停止当前工作，并丢弃排队的工作 |
| `decision.answer {decisionId, choice: approve/reject, clientId}` | 回答决定（去重、幂等） |
| `item.action {itemId, action, minutes?}` | `done` / `drop` / `snooze` / `checkNow` / `follow` / `ignore` |
| `memory.add/update/delete/restore` | 记忆编辑；删除保留墓碑，阻止相似内容被写回 |
| `profile.update`、`settings.update` | 局部更新 |
| `push.register {token, env}` | 上报 APNs token |
| `push.test`、`sessions.analyze`、`device.unpair`、`ping` | |
| 仅 Mac 本机：`pairing.start/cancel/confirm/state`、`devices.revoke`、`status.refresh`、`codex.login`、`brief.now` | |

Mac App 用 daemon 写在数据目录的 `local-client.json`（权限 0600）作为本机设备凭据，走同一协议。

## Codex 引擎

- daemon 启动 `codex app-server`（stdio JSON-RPC），按需重启。
- 所有工作串行执行：聊天消息、事项检查、早间汇报、用户对问题的回答。
- 聊天使用一个按天轮换的持久线程；记忆或 Profile 被修改后也会换新线程。主动检查和汇报使用临时线程（`ephemeral`）。
- 每轮输入前会附上上下文：当前时间、Profile、记忆、“不要再记”、进行中的事项。
- 权限档位映射：只读 → `read-only`；自动 · 仅工作区 → `workspace-write`（授权文件夹为可写根目录）；完全访问 → `danger-full-access`；审批策略 `on-request`。
- Codex 的审批请求变成“决定”卡片并推送，用户在手机上同意或拒绝；超时（默认 48 小时）按拒绝处理。
- 会话分析：`thread/list` 读取本机最近 7 天的 CLI / VS Code / exec 会话（排除 LoopMe 自己的线程和已推荐过的会话），由 Agent 挑出最多 3 件建议跟进的事。

## 调度与通知

- 每 30 秒检查：到期的事项检查、早间汇报（默认工作日 09:00，超过 3 小时不再补发）、暂停到期、决定超时、安静时段后补发通知。
- 事项连续 10 次检查无变化 → 发一个“是否继续跟进”的决定。
- 通知只在三种情况发：需要你决定、交代的事有结果、执行失败。限额：同一事项每天 2 次（决定不受限），全局每小时 5 条，超出后合并；安静时段（默认 22:00–08:00）内延后。锁屏默认只显示“你的 Agent 有新消息”。
