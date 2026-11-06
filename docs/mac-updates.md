# Mac 版自动更新（Sparkle + GitHub Releases）

iOS 版走 TestFlight。Mac 版不能上 TestFlight（要求 App 沙盒，而 LoopMe 需要启动 codex、写 `~/.codex`），所以用免费开源的 [Sparkle 2](https://sparkle-project.org)，安装包和更新信息都放在本仓库的 GitHub Releases 上。仓库需要是公开的（Sparkle 下载时不带凭据）。

## 谁负责什么

| 部分 | 负责什么 |
| --- | --- |
| Sparkle（App 内，`apple/Mac/Updater.swift`） | 每天检查一次 `https://github.com/Sailfishc/LoopMe/releases/latest/download/appcast.xml`；有新版本就后台下载，校验 EdDSA 签名，提示「安装并重启」。安装前先停掉 daemon，新 App 带着新 daemon 启动 |
| 发版脚本（`apple/scripts/release-mac.sh`） | 归档、Developer ID 签名、公证、生成并签名 `appcast.xml`、用 `gh` 发 GitHub Release |
| GitHub Releases | 每个版本一个 Release（tag `mac-v<版本>-<build>`），附件是 `LoopMe-<版本>-<build>.zip` 和 `appcast.xml` |
| 苹果公证 | macOS 才会放行下载来的 App |

`releases/latest/download/appcast.xml` 总是跳转到最新 Release 里的那份 appcast，所以不存在覆盖文件、CDN 缓存的问题：旧版本的文件原样保留，最新版本一发布就生效。Debug 构建和没有填公钥的构建不会开启更新。

## 一次性准备

1. **更新密钥**：第一次构建后，Sparkle 的工具在 `apple/build/SourcePackages/artifacts/sparkle/Sparkle/bin/`。
   ```sh
   ./generate_keys          # 私钥存进你的登录钥匙串，打印公钥
   ./generate_keys -x ~/loopme-sparkle-private-key   # 导出一份备份，放在安全的地方，不要进仓库
   ```
   把打印出的公钥填到 `apple/project.yml` 的 `LOOPME_UPDATE_PUBLIC_KEY`（公钥不是秘密，可以提交）。私钥丢了，已安装的 App 就再也收不到更新。
2. **公证凭据**：
   ```sh
   xcrun notarytool store-credentials loopme-notary --apple-id <Apple ID> --team-id 35KYWWD972
   ```
3. **GitHub CLI**：`brew install gh && gh auth login`，账号要能在本仓库发 Release。
4. 可选：新建 `apple/release.env`（已在 `.gitignore`）：
   ```sh
   NOTARY_PROFILE=loopme-notary
   ```

## 发一个版本

1. 在 `apple/project.yml` 里把 `CURRENT_PROJECT_VERSION` 加 1（Sparkle 按它比较新旧），需要时改 `MARKETING_VERSION`。
2. 运行：
   ```sh
   RELEASE_NOTES="修复了……" apple/scripts/release-mac.sh
   ```
   先只想检查打包和公证：`DRY_RUN=1 apple/scripts/release-mac.sh`。
3. 脚本先以草稿上传 zip 和 appcast，再把 Release 设为最新，所以「最新」不会指向一个还没有 appcast 的 Release。
4. 第一次安装：从 Release 页面下载 zip，解压后拖进「应用程序」。之后每台 Mac 会在一天内收到更新提示，也可以在 Mac 面板点「检查更新」。

## 注意

- 本仓库的「最新 Release」必须是 Mac 版。以后如果要发别的 Release（比如源码版本），发的时候不要勾选 latest，否则 Mac 版会找不到 appcast。
- 每个 appcast 只含当前这一个版本，不做增量包（安装包很小）。
- iOS 和 Mac 共用 `CURRENT_PROJECT_VERSION`，只增不减即可。
- 不进仓库：Sparkle 私钥及其导出文件、公证凭据、APNs 的 `.p8` 和 `apns.json`。别人 fork 了仓库也发不了更新给你的用户：没有你的私钥和 Developer ID 证书，签名对不上，App 会拒绝安装。

## 更新时 App 做了什么

1. Sparkle 下载、校验签名后，安装前先停掉 daemon（它的 Relay 子进程跟着退出）。
2. 新 App 启动新 daemon，并把自己的 build 号通过 `LOOPME_APP_BUILD` 传给它，daemon 写到 `daemon.build`。
3. 如果 App 曾被强制退出，旧 daemon 还在跑，新 App 看到 `daemon.build` 不是自己的版本，会先结束旧 daemon 再启动新的。手动用 `cargo run` 起的 daemon 不写这个文件，不受影响。
4. Relay 子进程监听 daemon 的标准输入，daemon 一旦退出（包括被强杀），Relay 也退出，不会占着 8443 端口。

Mac 面板底部显示当前版本，可以手动「检查更新」，也可以打开「自动安装更新」（下载后在退出时静默安装，默认关闭）。
