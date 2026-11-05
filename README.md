# LoopMe

住在你 Mac 上、会主动跟进事情的个人 Agent。你在 iPhone 上和它聊天、接收汇报、替它做决定。

- 产品与决策记录：[docs/product-design.md](docs/product-design.md)
- 架构与协议：[docs/architecture.md](docs/architecture.md)
- 本地构建与联调：[docs/build.md](docs/build.md)

```
daemon/   Rust daemon（Codex 引擎、存储、配对、调度、APNs）
apple/    Swift：LoopMeKit 共享包、iOS App、Mac 菜单栏 App（XcodeGen 工程）
tools/    假 Codex、Python 参考客户端、端到端测试
```
