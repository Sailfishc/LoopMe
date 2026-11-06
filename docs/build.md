# 本地构建与联调

目标：在一台 Apple Silicon Mac（macOS 14+）上从源码构建 daemon、Mac 菜单栏 App 和 iOS App，处理签名，然后真机联调。

## 0. 准备

| 工具 | 安装 |
| --- | --- |
| Xcode 15.3 及以上（含 iOS 17 SDK） | App Store |
| Rust（stable） | `curl https://sh.rustup.rs -sSf \| sh` |
| XcodeGen | `brew install xcodegen` |
| Codex CLI，并已登录 | `npm i -g @openai/codex` 或 `brew install codex`，然后 `codex login` |

## 1. 先单独跑 daemon（可选，但最快发现问题）

```bash
cd daemon
cargo test                     # 单元测试，包括与 Swift 共用的加密测试向量
cargo run -- --data-dir /tmp/loopme-dev --port 7788
```

不想消耗 Codex 额度时，可以用假的 Codex 跑完整流程：

```bash
python3 -m pip install websockets cryptography
LOOPME_CODEX="$PWD/../tools/mock-codex.py" cargo run -- --data-dir /tmp/loopme-dev
python3 ../tools/e2e_test.py          # 配对、对话、事项、记忆、审批、回复引用、Codex hook、代为授权、让我来、解除配对，全部自动验证
```

`tools/loopme_client.py` 是协议的参考客户端，可以在命令行里配对、发消息（见文件头说明）。

## 2. 生成 Xcode 工程

1. 打开 `apple/project.yml`，修改三处：
   - `settings.base.DEVELOPMENT_TEAM`：你的 Team ID（Xcode → Settings → Accounts 里可以看到）。
   - `LoopMe` 目标的 `PRODUCT_BUNDLE_IDENTIFIER`（默认 `com.example.loopme.ios`）。
   - `LoopMeMac` 目标的 `PRODUCT_BUNDLE_IDENTIFIER`（默认 `com.example.loopme.mac`）。
2. 生成工程：

```bash
cd apple
xcodegen generate
open LoopMe.xcodeproj
```

工程里有两个 scheme：`LoopMe`（iOS）和 `LoopMeMac`（菜单栏 App）。共享代码在本地 Swift 包 `apple/LoopMeKit` 里。

> 说明：这些 Swift 代码是在没有 Xcode 的环境里写的，**没有经过编译器检查**。第一次构建如果有编译错误，大概率是小问题（类型推断、API 名称），可以直接把报错发给我。Rust daemon 和协议已经过完整测试。

## 3. Mac 菜单栏 App

- 选 `LoopMeMac` scheme，Run。构建阶段会自动执行 `apple/scripts/build-daemon.sh`：用 cargo 编译 daemon，复制到 `LoopMe.app/Contents/MacOS/loopme-daemon` 并签名。
- App 启动后会拉起 daemon，数据目录是 `~/Library/Application Support/LoopMe`（数据库、密钥、日志 `daemon.log`）。
- 如果你已经在终端里用 `cargo run` 跑着 daemon（同一个数据目录），App 会直接连上它，不再另起一个。
- 菜单栏面板里：Codex 显示版本和是否登录（未登录点“登录”会打开终端运行 `codex login`）；“连接手机”显示二维码。

### 分发签名（Developer ID + 公证）

1. Xcode → Product → Archive（`LoopMeMac`）→ Distribute App → Developer ID → Upload（公证）。
2. 如果导出时提示内嵌的 `loopme-daemon` 签名不合规，可以手动重签后再公证：

```bash
APP="LoopMe.app"
ID="Developer ID Application: 你的名字 (TEAMID)"
codesign --force --options runtime --timestamp --sign "$ID" "$APP/Contents/MacOS/loopme-daemon"
codesign --force --options runtime --timestamp --sign "$ID" "$APP"
ditto -c -k --keepParent "$APP" LoopMe.zip
xcrun notarytool submit LoopMe.zip --keychain-profile <你的 notary profile> --wait
xcrun stapler staple "$APP"
```

## 4. iOS App

- 选 `LoopMe` scheme，连上 iPhone，Run。需要 iOS 17+。
- 第一次需要在 Signing & Capabilities 里确认 Push Notifications 能力已打开（`aps-environment` 已写在 `apple/iOS/LoopMe.entitlements`）。
- TestFlight：Product → Archive → Distribute App → TestFlight & App Store。Release 构建会以 `production` 环境注册推送。

## 5. 推送（Mac 直接调 APNs）

1. 在 Apple Developer → Keys 新建一个 APNs Auth Key，下载 `AuthKey_XXXXXX.p8`。
2. 在数据目录放一个 `apns.json`（Mac App 面板 → ⋯ → 打开数据目录）：

```json
{
  "teamId": "你的 Team ID",
  "keyId": "XXXXXX",
  "keyPath": "~/Library/Application Support/LoopMe/AuthKey_XXXXXX.p8",
  "topic": "你的 iOS bundle id"
}
```

3. 面板 ⋯ → 重启 daemon。面板“推送”一行会显示“APNs 已配置”。
4. 手机设置页 → “发送测试推送”。从 Xcode 直接运行的 Debug 版用 sandbox 环境，TestFlight 版用 production 环境，daemon 会按手机上报的环境自动选择。

## 6. 联调步骤

1. Mac 上运行 LoopMeMac；面板显示 Codex 版本和“就绪”。
2. 面板 → “连接手机”，出现二维码。
3. iPhone 上打开 LoopMe → “扫码连接”（也可以用系统相机扫，二维码是 `loopme://` 链接）。手机和 Mac 需在同一网络或手机连上公司 VPN。
4. 两边会显示同一个 4 位校验码，在 Mac 上点“确认配对”。
5. 在手机上发“帮我盯着 xxx”，应看到流式回复和一张事项卡。
6. 设置页里试“发送测试推送”、改权限档位、编辑记忆。
7. Mac 面板里点“接入”（让 LoopMe 看到你自己的 Codex 会话）。之后 `~/.codex/hooks.json` 里会多出 LoopMe 的 6 条 hook，面板显示“已接入”。
8. 在终端里随便开一个 `codex` 会话发一句话，手机顶部的“现在”条会出现“你的 Codex：…”。
9. 离开 Mac 5 分钟以上（或在数据目录写 `echo away > ~/Library/Application\ Support/LoopMe/presence` 模拟），让 Codex 请求一个需要授权的命令，手机上会出现“你的 Codex 在等授权”。测完删掉 `presence` 文件。
10. 会话停在失败的测试时（你不在 Mac 前），约 2 分钟后手机上会出现“可以接手”卡片；点“让我来”，结果卡里可以看改动、推送或撤销。你自己的工作区不会被改。

### 常见问题

- **手机连不上 Mac**：二维码里带了 Mac 的所有 IPv4 地址和 `.local` 名称，手机会依次尝试。VPN 地址不在其中时，可以在启动 daemon 前设置环境变量 `LOOPME_HOSTS=10.x.x.x`（多个用逗号分隔），或在终端用 `LOOPME_HOSTS=... cargo run` 启动。另外检查 macOS 防火墙是否允许 `loopme-daemon` 接受连接（端口 7788）。
- **iOS 提示本地网络权限**：允许即可；拒绝后需要在 设置 → 隐私 → 本地网络 里打开。
- **Codex 找不到**：daemon 依次查找设置里的路径、`LOOPME_CODEX`、登录 shell 里的 `codex`、`/opt/homebrew/bin`、`/usr/local/bin` 等。
- **hook 没反应**：看 Mac 面板“你的 Codex 会话”是不是“已接入”；`codex` 版本需要 0.166 以上。Codex 只运行标记过信任的 hook，LoopMe 换位置后会在 10 分钟内自动重新标记，也可以在面板里点“重新接入”。
- **看日志**：`~/Library/Application Support/LoopMe/daemon.log`，或终端里 `RUST_LOG=debug cargo run`。
