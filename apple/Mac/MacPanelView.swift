import LoopMeKit
import ServiceManagement
import SwiftUI

/// The menu bar panel: connection, QR pairing, Codex status, agent state.
struct MacPanelView: View {
    @EnvironmentObject var store: LoopMeStore
    @EnvironmentObject var daemon: DaemonController
    @EnvironmentObject var updater: AppUpdater
    @EnvironmentObject var keepAwake: KeepAwake
    @Environment(\.openWindow) private var openWindow
    @State private var launchAtLogin = SMAppService.mainApp.status == .enabled

    private var phones: [Device] { store.devices.filter { !$0.isLocal } }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            header
            Divider().overlay(Theme.border)
            pairingSection
            Divider().overlay(Theme.border)
            codexSection
            hooksSection
            agentSection
            networkSection
            Divider().overlay(Theme.border)
            toggles
            footer
        }
        .padding(16)
        .frame(width: 340)
        .background(Theme.background)
        .onAppear {
            store.refreshPairing()
            store.refreshStatus()
        }
    }

    // MARK: header

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: "circle.circle").font(.title2).foregroundStyle(Theme.text)
            VStack(alignment: .leading, spacing: 2) {
                Text("LoopMe").font(.headline).foregroundStyle(Theme.text)
                HStack(spacing: 5) {
                    Circle().fill(store.isConnected ? Theme.green : Theme.warning).frame(width: 7, height: 7)
                    Text(store.isConnected ? (phones.isEmpty ? "运行中 · 还没有连接手机" : "运行中 · 已配对 \(phones.count) 台手机") : "daemon：\(daemon.state)")
                        .font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                }
            }
            Spacer()
        }
    }

    // MARK: pairing

    @ViewBuilder private var pairingSection: some View {
        switch store.pairing.state {
        case "waiting":
            VStack(spacing: 10) {
                if let url = store.pairing.url, let img = QRCode.image(url) {
                    Image(nsImage: img)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 190, height: 190)
                        .padding(10)
                        .background(Color.white, in: RoundedRectangle(cornerRadius: 12))
                }
                Text("用 iPhone 上的 LoopMe 扫码").font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                if let exp = store.pairing.expiresAt {
                    Text("二维码 \(Fmt.relative(exp))失效 · 手机需在同一网络或公司 VPN").font(.caption).foregroundStyle(Theme.secondary)
                }
                HStack {
                    Button("复制配对链接") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(store.pairing.url ?? "", forType: .string)
                    }
                    .buttonStyle(PillButtonStyle(prominent: false))
                    Button("取消") { store.cancelPairing() }.buttonStyle(PillButtonStyle(prominent: false))
                }
            }
            .frame(maxWidth: .infinity)
        case "confirm":
            VStack(spacing: 10) {
                Text("\(store.pairing.deviceName ?? "手机") 请求连接").font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                Text(store.pairing.sas ?? "----")
                    .font(.system(size: 44, weight: .semibold, design: .rounded).monospacedDigit())
                    .tracking(8)
                    .foregroundStyle(Theme.text)
                Text("确认手机上显示的是同样的 4 位数字").font(.caption).foregroundStyle(Theme.secondary)
                HStack {
                    Button("确认配对") { store.confirmPairing(true) }.buttonStyle(PillButtonStyle())
                    Button("拒绝") { store.confirmPairing(false) }.buttonStyle(PillButtonStyle(prominent: false))
                }
            }
            .frame(maxWidth: .infinity)
        default:
            VStack(alignment: .leading, spacing: 8) {
                if store.pairing.state == "paired", let name = store.pairing.deviceName {
                    Label("已连接 \(name)", systemImage: "checkmark.circle.fill").font(.caption).foregroundStyle(Theme.green)
                } else if store.pairing.state == "failed" {
                    Label(store.pairing.error ?? "配对失败", systemImage: "xmark.circle").font(.caption).foregroundStyle(Theme.secondary)
                }
                ForEach(phones) { d in
                    HStack {
                        Image(systemName: "iphone").foregroundStyle(Theme.secondary)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(d.name).font(.subheadline).foregroundStyle(Theme.text)
                            Text([d.lastSeenAt.map { "最近在线 \(Fmt.relative($0))" }, d.hasPush ? "推送已开启" : "未开启推送"]
                                .compactMap { $0 }.joined(separator: " · "))
                                .font(.caption2).foregroundStyle(Theme.secondary)
                        }
                        Spacer()
                        Button("解除") { store.revoke(d) }.buttonStyle(.plain).font(.caption).foregroundStyle(Theme.secondary)
                    }
                }
                Button {
                    store.startPairing()
                } label: {
                    Text(phones.isEmpty ? "连接手机" : "再连接一台手机").frame(maxWidth: .infinity)
                }
                .buttonStyle(PillButtonStyle(prominent: phones.isEmpty))
                .disabled(!store.isConnected)
            }
        }
    }

    // MARK: status rows

    private var codexSection: some View {
        let c = store.status.codex
        return row(icon: "terminal", title: "Codex") {
            if !c.found {
                Text("未安装").foregroundStyle(Theme.warning)
            } else if c.loggedIn == false {
                HStack(spacing: 6) {
                    Text("\(c.version ?? "") · 未登录").foregroundStyle(Theme.warning)
                    Button("登录") { store.codexLogin() }.font(.caption.weight(.semibold))
                }
            } else {
                Text("\(c.version ?? "已安装") · 就绪\(fromApp ? " · 用 Codex App 自带的" : "")").foregroundStyle(Theme.secondary)
                    .help(c.path ?? "")
            }
        }
    }

    /// No CLI installed: LoopMe runs the codex binary inside Codex.app.
    private var fromApp: Bool { store.status.codex.path?.contains(".app/") == true }

    /// The one-time confirm for watching the user's own Codex sessions.
    @ViewBuilder private var hooksSection: some View {
        if store.status.codex.found {
            if store.settings.codexHooks {
                row(icon: "eye", title: "你的 Codex 会话") {
                    if store.status.hooks == "on" {
                        Text("已接入").foregroundStyle(Theme.secondary)
                    } else {
                        HStack(spacing: 6) {
                            Text(store.status.hooks == "error" ? "出错" : "需要确认").foregroundStyle(Theme.warning)
                            Button("重新接入") { store.enableCodexHooks() }.font(.caption.weight(.semibold))
                        }
                    }
                }
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    Text("让 LoopMe 看到你自己的 Codex 会话？").font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    Text("会话停下、卡住或等授权时，你不在 Mac 前它才来找你。会在 ~/.codex/hooks.json 加几条 hook 并标记信任，只问这一次，可在设置里关掉。")
                        .font(.caption).foregroundStyle(Theme.secondary)
                    Button("接入") { store.enableCodexHooks() }
                        .buttonStyle(PillButtonStyle())
                        .disabled(!store.isConnected)
                }
                .loopmeCard()
            }
        }
    }

    /// The same "现在" list as on the phone.
    private var agentSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            row(icon: "sparkles", title: "现在") {
                Text(agentText).foregroundStyle(Theme.secondary)
            }
            if store.nowSummary != nil {
                NowPanel()
            }
        }
    }

    private var agentText: String {
        switch store.status.agent {
        case "working": return "工作中"
        case "paused": return "已暂停"
        case "error": return "出错"
        default: return "空闲"
        }
    }

    private var relayText: String {
        let port = String(store.status.relayPort)
        switch store.status.relay {
        case "on": return "Relay 运行中 · 端口 \(port)"
        case "connecting": return "Relay 启动中…"
        case "error": return "Relay 出错，看日志"
        default: return "直连 · 端口 \(String(store.status.listenPort))"
        }
    }

    private var networkSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            row(icon: "network", title: "手机连接") {
                Text(relayText).foregroundStyle(store.status.relay == "error" ? Theme.danger : Theme.secondary)
            }
            if !store.status.hosts.isEmpty {
                Text(store.status.hosts.joined(separator: "  ")).font(.caption2.monospaced()).foregroundStyle(Theme.secondary)
                    .lineLimit(2).padding(.leading, 26)
                    .help("手机按这个顺序尝试这些地址")
            }
            DisclosureGroup("对外地址") {
                VStack(alignment: .leading, spacing: 4) {
                    Text("IT 给这台 Mac 分配了固定 IP 或内网域名时填在这里，二维码会把它排在最前。改完需要手机重新扫码。")
                        .font(.caption2).foregroundStyle(Theme.secondary)
                    FolderListEditor(title: "", key: "relayHosts", folders: store.settings.relayHosts, placeholder: "loopme-mac.corp.example 或 10.20.30.40")
                }
                .padding(.top, 4)
            }
            .font(.caption)
            .padding(.leading, 26)
            row(icon: "bell", title: "推送") {
                Text(store.status.pushConfigured ? "APNs 已配置" : "未配置 apns.json").foregroundStyle(Theme.secondary)
            }
        }
    }

    private var toggles: some View {
        VStack(alignment: .leading, spacing: 8) {
            toggleRow("插电时保持唤醒", isOn: $keepAwake.enabled)
            toggleRow("暂停主动工作", isOn: Binding(get: { store.settings.paused }, set: { v in store.updateSettings(["paused": v]) }))
                .disabled(!store.isConnected)
            toggleRow("登录时启动", isOn: Binding(get: { launchAtLogin }, set: { v in
                do {
                    if v { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
                } catch {
                    store.lastError = error.localizedDescription
                }
                launchAtLogin = SMAppService.mainApp.status == .enabled
            }))
            if updater.enabled {
                toggleRow("自动检查更新", isOn: Binding(get: { updater.automaticallyChecks }, set: { updater.automaticallyChecks = $0 }))
                toggleRow("自动安装更新", isOn: Binding(get: { updater.automaticallyInstalls }, set: { updater.automaticallyInstalls = $0 }))
                    .disabled(!updater.automaticallyChecks)
            }
            HStack {
                Text("版本 \(AppUpdater.versionText)").foregroundStyle(Theme.secondary)
                Spacer()
                if updater.enabled {
                    Button("检查更新") { updater.checkForUpdates() }
                        .buttonStyle(.plain)
                        .foregroundStyle(Theme.accent)
                        .disabled(!updater.canCheck)
                }
            }
            .font(.caption)
        }
        .toggleStyle(.switch)
        .controlSize(.small)
        .font(.subheadline)
        .foregroundStyle(Theme.text)
    }

    private func toggleRow(_ title: String, isOn: Binding<Bool>) -> some View {
        HStack {
            Text(title)
            Spacer()
            Toggle(title, isOn: isOn).labelsHidden()
        }
    }

    private var footer: some View {
        HStack {
            Button("打开对话") { open("chat") }
            Button("设置") { open("settings") }
            Spacer()
            Menu {
                Button("现在整理今天的事") { store.briefNow() }
                if updater.enabled {
                    Button("检查更新…") { updater.checkForUpdates() }.disabled(!updater.canCheck)
                }
                Button("重启 daemon") { daemon.restart() }
                Button("打开日志") { NSWorkspace.shared.open(daemon.logURL) }
                Button("打开数据目录") { NSWorkspace.shared.open(daemon.dataDirectory) }
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .menuStyle(.button)
            .buttonStyle(.plain)
            .fixedSize()
            Button("退出") { NSApp.terminate(nil) }
        }

        .buttonStyle(.plain)
        .font(.subheadline)
        .foregroundStyle(Theme.text)
    }

    private func open(_ id: String) {
        openWindow(id: id)
        NSApp.activate(ignoringOtherApps: true)
    }

    private func row<V: View>(icon: String, title: String, @ViewBuilder value: () -> V) -> some View {
        HStack(spacing: 8) {
            Image(systemName: icon).frame(width: 18).foregroundStyle(Theme.secondary)
            Text(title).foregroundStyle(Theme.text)
            Spacer()
            value()
        }
        .font(.subheadline)
    }
}
