# LoopMe v0 架构与协议

产品决策见 [product-design.md](product-design.md)，构建步骤见 [build.md](build.md)。

## 组成

```
iPhone (LoopMe iOS) ──WebSocket + 端到端加密──┐
                                              ├── loopme-daemon (Rust, 跑在 Mac 上) ── codex app-server (子进程)
Mac 菜单栏 App ────── 同一协议, 127.0.0.1 ─────┘          │
                                                          ├── SQLite (~/Library/Application Support/LoopMe/loopme.db)
                                                          ├── APNs (直接调用)
你自己的 Codex 会话 ── hooks.json ── loopme-daemon hook ──┘   (事件文件写到 <数据目录>/inbox/)
```

| 目录 | 内容 |
| --- | --- |
| `daemon/` | Rust daemon：存储、协议、配对、Codex 引擎、调度、通知 |
| `apple/LoopMeKit/` | Swift 包：模型、加密、连接、状态（`LoopMeStore`）、共享界面（对话、卡片、设置） |
| `apple/iOS/` | iOS App：扫码连接、对话、设置、推送 |
| `apple/Mac/` | 菜单栏 App：启动 daemon、二维码配对、Codex 状态、对话窗口 |
| `tools/` | 假 Codex、Python 参考客户端、端到端测试 |

## 与设计文档的差异（v0）

- **网络层**：设计定的是 iroh + 内网 relay。现在用的是自己的轻量 Relay（见下文「手机怎么连到 Mac」）+ 应用层端到端加密。加密方案和传输层无关，以后换 iroh 只需替换传输层，协议和配对不变。
- **daemon 托管**：v0 由菜单栏 App 以子进程方式启动 daemon（退出 App 即停止）。设计中的 `SMAppService.agent`（LaunchAgent）放到下一步。
- **共享核心**：设计是 Rust `loopme-core` 通过 UniFFI 给 iOS 用。v0 的 iOS 端用 CryptoKit 原生实现同一套加密（两边用同一组测试向量校验），省去 UniFFI 构建链。
- **Agent 输出结构化信息**：用回复末尾的 `<loopme>{json}</loopme>` 控制块（对用户隐藏），而不是 Codex 的实验性 dynamic tools。

## 手机怎么连到 Mac（Relay）

手机不再直连 daemon，而是连 Mac 上的 `loopme-relay`。daemon 只在 `127.0.0.1:7788` 监听（给 Mac App），并主动连到 Relay。

```
手机 ──wss──▶ loopme-relay（0.0.0.0:8443，自签证书）◀──ws── loopme-daemon（127.0.0.1:8444 出站）
          └──────────── 端到端加密（X25519 + ChaCha20-Poly1305），Relay 只转发密文 ────────────┘
```

- **进程**：daemon 启动时用 `loopme-daemon relay --listen 0.0.0.0:8443 --mac-listen 127.0.0.1:8444` 拉起 Relay，退出就重启。`--relay-port 0` 回到旧的直连模式（daemon 监听 0.0.0.0）。
- **Relay 接口**（`daemon/src/relay.rs`）：
  - `GET /healthz?mac=<id>&t=<token>`：`{"ok":true,"macOnline":…}`，给手机的连接诊断用；不带凭据时不透露 Mac 是否在线。
  - `/v1/mac`（只在回环端口免令牌；公网端口要 `LOOPME_RELAY_MAC_TOKENS`）：daemon 注册 `{"t":"register","macId","phoneToken"}`。
  - `/v1/phone/<macId>/pair|session?t=<token>`：手机接入。Relay 给 daemon 发 `{"t":"incoming","conn","path"}`，daemon 再出站连 `/v1/mac/accept?conn=`，Relay 把两条 WebSocket 逐帧拼接。之后的协议和以前的 `/v1/pair`、`/v1` 完全一样。
  - 出错时先回一帧 `{"t":"error","code":"mac_offline"|"bad_token","message"}` 再关闭。
- **证书**：首次启动生成到 `<data>/relay/cert.der`。二维码带它的 SHA-256（`f`），手机只认这个指纹，不做 CA 和主机名校验。
- **令牌**：`phoneToken` 存在 kv（`relay_phone_token`），写进二维码（`t`）。它只用来挡住内网里的随意连接，设备身份仍由配对密钥证明。
- **二维码 v2**：`loopme://pair?v=2&k&c&h&p=<relay 端口>&n&r=<macId>&t=<token>&f=<指纹>`。`h` 依次是：设置里的「对外地址」（`relayHosts`）→ VPN 网卡地址 → 其他 IPv4 → `.local`。v1（没有 `r`）仍按直连处理。
- **手机**：同时尝试前几个地址（每个晚 250 ms 启动），先完成握手的胜出；网络或 VPN 变化时立即重连。「设置 → 连接诊断」逐个地址检查 TCP、Relay、Mac 在线，并给出一句结论。
- **挪到服务器**：`--relay-url ws://relay.corp:8444 --relay-hosts relay.corp`（加 `LOOPME_RELAY_MAC_TOKEN`）让 daemon 连别处的同一个 Relay，手机侧只是二维码里的地址变了。

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
   - 事件 `{"event","seq","data"}`：`message`、`messageDelta`（流式全文）、`item`、`decision`、`memory`、`profile`、`settings`、`status`、`devices`、`pairing`、`session`（你自己的 Codex 会话）、`resync`。

| 方法 | 说明 |
| --- | --- |
| `sync` | 全量快照（最近 300 条消息、事项、决定、记忆、Profile、设置、状态、设备、最近一天的 Codex 会话） |
| `message.send {clientId, text, replyTo?}` | 发消息；`clientId` 去重；`replyTo` 引用一条旧消息，作为上下文一起给 Agent |
| `message.feedback {messageId, value: up/down/""}` | 👍/👎；👎 过的消息会写进上下文，让 Agent 少发这类 |
| `offer.act {messageId, action, clientId}` | 点卡片上的按钮：`take`（让我来）、`mine`、`ignore`、`stop`、`push`、`undo` |
| `offer.diff {messageId}` | 结果卡的改动（patch 文本） |
| `app.opened` | App 到了前台；当天第一次会开始“今天的第一句话” |
| `run.stop` | 停止当前工作，并丢弃排队的工作 |
| `decision.answer {decisionId, choice: approve/reject/later, clientId}` | 回答决定（去重、幂等）；`later` = 回 Mac 再说 |
| `item.action {itemId, action, minutes?}` | `done` / `drop` / `snooze` / `checkNow` / `follow` / `ignore` |
| `memory.add/update/delete/restore` | 记忆编辑；删除保留墓碑，阻止相似内容被写回 |
| `profile.update`、`settings.update` | 局部更新 |
| `push.register {token, env}` | 上报 APNs token |
| `push.test`、`sessions.analyze`、`device.unpair`、`ping` | |
| 仅 Mac 本机：`pairing.start/cancel/confirm/state`、`devices.revoke`、`status.refresh`、`codex.login`、`brief.now`、`hooks.enable`、`hooks.disable` | |

Mac App 用 daemon 写在数据目录的 `local-client.json`（权限 0600）作为本机设备凭据，走同一协议。

## Codex 引擎

- daemon 启动 `codex app-server`（stdio JSON-RPC），按需重启。
- 所有工作串行执行：聊天消息、事项检查、早间汇报、用户对问题的回答。
- 聊天使用一个按天轮换的持久线程；记忆或 Profile 被修改后也会换新线程。主动检查和汇报使用临时线程（`ephemeral`）。
- 每轮输入前会附上上下文：当前时间、Profile、记忆、“不要再记”、进行中的事项。
- 权限档位映射：只读 → `read-only`；自动 · 仅工作区 → `workspace-write`（授权文件夹为可写根目录）；完全访问 → `danger-full-access`；审批策略 `on-request`。
- Codex 的审批请求变成“决定”卡片并推送，用户在手机上同意或拒绝；超时（默认 48 小时）按拒绝处理。
- 会话分析：`thread/list` 读取本机所有目录最近 7 天的 CLI / VS Code / exec 会话（排除 LoopMe 自己的线程和已推荐过的会话），由 Agent 挑出最多 3 件：能直接做完的提成“可以接手”卡片，只值得跟进的提成建议（一次只显示一条）。

## 你自己的 Codex 会话（hooks）

- Codex 0.166 会执行 `$CODEX_HOME/hooks.json`（默认 `~/.codex/hooks.json`）里的 command hook，但每条 hook 要先在用户的 `config.toml` 里标记信任（`hooks.state."<key>".trusted_hash`）才会运行。
- 一次确认：Mac 上点“接入”后，daemon 往 hooks.json 加 6 条 hook（`SessionStart`、`UserPromptSubmit`、`Stop`、`PermissionRequest`、`SessionEnd`、`Interrupt`），用户原有的 hook 不动；再通过 app-server 的 `hooks/list` + `config/batchWrite` 标记信任。之后每 10 分钟检查一次，App 换了位置会自动重新接入。关闭时只删 LoopMe 自己的条目。
- hook 命令就是 daemon 自己：`loopme-daemon hook --loopme-data-dir <数据目录>`。它把事件压缩后写到 `<数据目录>/inbox/`，立刻退出；daemon 每 0.5 秒收一次。LoopMe 自己启动的 Codex 带 `LOOPME_INTERNAL=1`，它的 hook 会被跳过。
- `PermissionRequest`：你不在 Mac 前时，变成手机上的“你的 Codex 在等授权”卡片（同意 / 拒绝 / 回 Mac 再说），hook 最多等 14 分钟；你回到 Mac 或选“回 Mac 再说”时，hook 不给答案，Codex 照常在 Mac 上问你。
- 设置里的“不看这些文件夹”下的会话会被忽略。

## 会话判断与打扰分级（`attention.rs`）

- 在不在 Mac 前：macOS 上读 `ioreg` 的 HID 空闲时间，超过 5 分钟算离开（数据目录下的 `presence` 文件写 `desk`/`away` 可以覆盖，用于测试）。“正在看 Codex”= 2 分钟内有输入且最前面是终端、编辑器或 Codex App（`lsappinfo`；`front` 文件可覆盖）。分不出终端里的哪个标签页，所以是“在看 Codex”而不是“在看这个会话”。
- 每次会话停下：带 `<…>` 的自动化提示（如 `<heartbeat>`）直接跳过；你不在 Mac 前，2 分钟合并窗口后让 Codex 只读看一眼；你在 Mac 前，只有这一轮跑了 10 分钟以上才看。会话 4 小时没动静且你不在 → 每天最多看一次。每天最多看 40 次。
- 判读用你自己的 Codex（临时只读线程），控制块里返回 `attention`：`stuck` / `question`（需要你）、`done` / `milestone`（成果）、`none`。卡住且能接手时另发一张 `takeover` 卡，`attention.messageId` 指向它。
- `PermissionRequest` 由规则直接定为 `approval`；Codex 继续、被打断或 15 分钟没动静时清掉。你在 Codex 里输入新提示时，旧卡片也清掉。
- 送达：需要你 → 不在 Mac 前推送到手机；在 Mac 前发 `alert` 事件，Mac App 弹系统横幅；正在看 Codex 时只更新卡片。成果 → 只更新卡片，不响铃。
- 节奏：同一会话同一类 30 分钟内不重复打断；每小时最多 4 次、每天 20 次，超出的只更新卡片；夜间免打扰只放行授权，其余早上合并成一条。
- 授权：在 Mac 前不弹横幅（Codex 自己就在 Mac 上问）。不在 Mac 前，同一会话 10 分钟内只推送第一条，后面的授权静默出现在“现在”条里，仍然可以在手机上同意或拒绝；授权不再写进对话。会话有新动静（Codex 继续、被打断、下一条授权）时，旧的授权卡片自动过期。授权不计入每小时和每天的次数。
- 手机：`session.ack`（好的/知道了，卡片变成对话里的一行记录）、`session.snooze`（稍后，默认 30 分钟）、`session.mute`（这个会话先别提醒）。

## 让我来

- 在 `<数据目录>/worktrees/` 下用 `git worktree` 新建分支 `loopme/<任务>-<日期>`，从你当前的 HEAD 分出；会话里未提交的改动先作为一个提交带过去。你的工作区和分支完全不动。
- Codex 只在这个 worktree 里写（可写根目录 = worktree + 仓库的 `.git`），改完跑测试并提交，不推送。
- 做完发一张结果卡：看改动（patch）、推送（`git push -u origin <分支>`，手机上会再确认一次）、撤销（删 worktree 和分支）。中途点停止会丢弃改动。

## “现在”条

- 客户端根据快照计算：`N 件等你 · M 个在跑`（待处理的决定 + 需要你的会话）> 正在做（LoopMe 自己的任务）> 成果 > `M 个在跑 · 没有要你做的` > 在盯（进行中的事项）。展开后依次是等你决定、等你、成果、正在做、在跑（默认折叠）、在盯。不显示下次检查时间。

## 调度与通知

- 每 30 秒检查：到期的事项检查、暂停到期、决定超时、安静时段后补发通知。
- 今天的第一句话：当天第一次回到 Mac（5 点以后）或打开 App 时发，默认只在工作日。
- 检查频率由 Agent 按等待的东西给出（外部结果约 5 分钟、review 约 30 分钟、其它约 3 小时）；没有变化时间隔翻倍，最长 2 小时（Agent 给的更长时以它为准）。
- 事项连续 10 次检查无变化 → 发一个“是否继续跟进”的决定。
- 通知只在三种情况发：需要你决定、交代的事有结果、执行失败。限额：同一事项每天 2 次（决定不受限），全局每小时 5 条，超出后合并；安静时段（默认 22:00–08:00）内延后。锁屏默认只显示“你的 Agent 有新消息”。
