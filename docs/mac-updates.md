# Mac 版自动更新（Sparkle）

iOS 版走 TestFlight。Mac 版不能上 TestFlight（要求 App 沙盒，而 LoopMe 需要启动 codex、写 `~/.codex`），所以用免费开源的 [Sparkle 2](https://sparkle-project.org)。更新包放在公司内网静态存储上，只有连着内网或 VPN 的 Mac 能更新。

## 谁负责什么

| 部分 | 负责什么 |
| --- | --- |
| Sparkle（App 内，`apple/Mac/Updater.swift`） | 每天检查一次 `SUFeedURL` 指向的 `appcast.xml`；有新版本就后台下载，校验 EdDSA 签名，提示「安装并重启」。安装前先停掉 daemon，新 App 带着新 daemon 启动。菜单里有「检查更新…」 |
| 发版脚本（`apple/scripts/release-mac.sh`） | 归档、Developer ID 签名、公证、生成并签名 `appcast.xml`、（可选）上传 |
| 内网存储 | 只放静态文件：`appcast.xml` 和每个版本的 zip |
| 苹果公证 | macOS 才会放行下载来的 App |

没有设置 `LOOPME_UPDATE_FEED_URL` 的构建（平时本地开发）不会开启更新。

## 一次性准备

1. **更新密钥**：第一次构建后，Sparkle 的工具在 `apple/build/SourcePackages/artifacts/sparkle/Sparkle/bin/`。
   ```sh
   ./generate_keys          # 私钥存进你的登录钥匙串，打印公钥
   ```
   公钥给 `LOOPME_UPDATE_PUBLIC_KEY`。私钥丢了就没法再发更新（已安装的 App 只认这把钥匙），用 `generate_keys -x 文件` 导出一份备份，放在安全的地方。
2. **公证凭据**：
   ```sh
   xcrun notarytool store-credentials loopme-notary --apple-id <Apple ID> --team-id 35KYWWD972
   ```
3. **存储地址**：确定内网上的目录，例如 `https://files.corp.example/loopme/mac/`。`appcast.xml` 和 zip 放在同一目录。

## 开源仓库要注意的

代码公开没关系：更新的安全靠两把不在仓库里的钥匙，一把是 Sparkle 私钥（在你的钥匙串里），一把是 Developer ID 证书。别人 fork 后只能给自己的用户发更新，发不到你的用户手上；就算有人改了 CDN 上的文件，签名对不上，App 也会拒绝安装。

- 不进仓库：Sparkle 私钥及其导出文件、公证凭据、APNs 的 `.p8` 和 `apns.json`（已加入 `.gitignore`）。
- 内网 CDN 地址和上传命令写在 `apple/release.env`（不提交），脚本会自动读取：
  ```sh
  LOOPME_UPDATE_FEED_URL=https://cdn.corp.example/loopme/mac/appcast.xml
  LOOPME_UPDATE_PUBLIC_KEY=<公钥>
  NOTARY_PROFILE=loopme-notary
  UPLOAD_CMD='<同步 "$UPDATES_DIR" 到 CDN 的命令>'
  ```
- 公钥、Team ID、Bundle ID 不是秘密，公开没有问题。

## 发一个版本

1. 在 `apple/project.yml` 里把 `CURRENT_PROJECT_VERSION` 加 1（Sparkle 按它比较新旧），需要时改 `MARKETING_VERSION`。
2. 运行：
   ```sh
   LOOPME_UPDATE_FEED_URL=https://files.corp.example/loopme/mac/appcast.xml \
   LOOPME_UPDATE_PUBLIC_KEY=<公钥> \
   NOTARY_PROFILE=loopme-notary \
   RELEASE_NOTES="修复了……" \
   UPLOAD_CMD='<把 "$UPDATES_DIR" 同步到存储目录的命令>' \
   apple/scripts/release-mac.sh
   ```
   `apple/build/updates/` 会保留所有发过的 zip，`generate_appcast` 每次根据它重新生成 `appcast.xml`（也会生成增量包）。不设 `UPLOAD_CMD` 时，脚本最后会提示要上传哪些文件。
3. 第一次安装需要手动分发 zip。之后每台 Mac 会在一天内收到更新提示，也可以在菜单里点「检查更新…」。

## 交给发版流程的约定

上传 CDN 和发版自动化由另一边负责。App 和它之间只靠下面这些约定，改动前要两边对齐：

| 约定 | 内容 |
| --- | --- |
| Feed 地址 | `LOOPME_UPDATE_FEED_URL`，写进 App 后就固定了。已安装的 App 只会去这个地址找更新，**发布后不要改**；要换地址，得先发一个指向新地址的版本 |
| 目录 | `appcast.xml` 和各版本 zip 放在同一目录（或 `DOWNLOAD_URL_PREFIX` 指向的目录）。旧 zip 保留，增量包要靠它们 |
| 文件 | `LoopMe-<版本>-<build>.zip`（ditto 打包、已公证并 staple 的 `LoopMe.app`）、`appcast.xml`（`generate_appcast` 生成并用 EdDSA 签名）、可选的同名 `.txt` 更新说明、`*.delta` 增量包 |
| 版本号 | 每次发布 `CURRENT_PROJECT_VERSION` 必须变大，Sparkle 只比较它 |
| 上传顺序 | 先传 zip 和 delta，最后传 `appcast.xml`，避免 App 读到新 appcast 时 zip 还不存在 |
| 缓存 | `appcast.xml` 在 CDN 上的缓存不要超过 10 分钟（或上传后主动刷新），zip 可以长期缓存（文件名带版本，不会被覆盖） |
| 访问 | HTTPS；只需要内网和 VPN 能访问，不要求鉴权（Sparkle 不带凭据） |
| 接入点 | `release-mac.sh` 的 `UPLOAD_CMD`，执行时环境变量 `UPDATES_DIR` 指向要同步的目录 |
| 密钥 | Sparkle 私钥只在发版那台 Mac 的钥匙串里（或 `generate_appcast --ed-key-file` 指向的文件，不进仓库） |

## 更新时 App 做了什么

1. Sparkle 下载、校验签名后，安装前先停掉 daemon（它的 Relay 子进程跟着退出）。
2. 新 App 启动新 daemon，并把自己的 build 号通过 `LOOPME_APP_BUILD` 传给它，daemon 写到 `daemon.build`。
3. 如果 App 曾被强制退出，旧 daemon 还在跑，新 App 看到 `daemon.build` 不是自己的版本，会先结束旧 daemon 再启动新的。手动用 `cargo run` 起的 daemon 不写这个文件，不受影响。
4. Relay 子进程监听 daemon 的标准输入，daemon 一旦退出（包括被强杀），Relay 也退出，不会占着 8443 端口。

Mac 面板底部显示当前版本，可以手动「检查更新」，也可以打开「自动安装更新」（下载后在退出时静默安装，默认关闭）。

## 注意

- iOS 和 Mac 共用 `CURRENT_PROJECT_VERSION`，只增不减即可。
- 从终端用 `cargo run` 起的 daemon 不归 App 管，更新时不会被停掉。
