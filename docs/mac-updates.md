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

## 注意

- iOS 和 Mac 共用 `CURRENT_PROJECT_VERSION`，只增不减即可。
- 从终端用 `cargo run` 起的 daemon 不归 App 管，更新时不会被停掉。
