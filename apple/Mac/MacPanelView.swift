import LoopMeKit
import ServiceManagement
import SwiftUI

/// The menu bar panel: connection, QR pairing, Codex status, agent state.
struct MacPanelView: View {
    @EnvironmentObject var store: LoopMeStore
    @EnvironmentObject var daemon: DaemonController
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
                Text("\(c.version ?? "已安装") · 就绪").foregroundStyle(Theme.secondary)
            }
        }
    }

    private var agentSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            row(icon: "sparkles", title: "Agent") {
                HStack(spacing: 6) {
                    Text(agentText).foregroundStyle(Theme.secondary)
                    if store.isWorking {
                        Button("停止") { store.stopRun() }.font(.caption.weight(.semibold))
                    }
                }
            }
            if let run = store.status.currentRun {
                Text(run.title).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1).padding(.leading, 26)
            }
            if let t = store.status.nextWakeAt {
                row(icon: "alarm", title: "下次唤醒") { Text(Fmt.time(t)).foregroundStyle(Theme.secondary) }
            }
            if !store.pendingDecisions.isEmpty {
                row(icon: "hand.raised", title: "需要你") {
                    Text("\(store.pendingDecisions.count) 件").foregroundStyle(Theme.warning)
                }
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

    private var networkSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            row(icon: "network", title: "连接方式") {
                Text("直连 · 端口 \(String(store.status.listenPort))").foregroundStyle(Theme.secondary)
            }
            if !store.status.hosts.isEmpty {
                Text(store.status.hosts.joined(separator: "  ")).font(.caption2.monospaced()).foregroundStyle(Theme.secondary)
                    .lineLimit(2).padding(.leading, 26)
            }
            row(icon: "bell", title: "推送") {
                Text(store.status.pushConfigured ? "APNs 已配置" : "未配置 apns.json").foregroundStyle(Theme.secondary)
            }
        }
    }

    private var toggles: some View {
        VStack(alignment: .leading, spacing: 8) {
            Toggle("插电时保持唤醒", isOn: $keepAwake.enabled)
            Toggle("暂停主动工作", isOn: Binding(get: { store.settings.paused }, set: { v in store.updateSettings(["paused": v]) }))
                .disabled(!store.isConnected)
            Toggle("登录时启动", isOn: Binding(get: { launchAtLogin }, set: { v in
                do {
                    if v { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
                } catch {
                    store.lastError = error.localizedDescription
                }
                launchAtLogin = SMAppService.mainApp.status == .enabled
            }))
        }
        .toggleStyle(.switch)
        .controlSize(.small)
        .font(.subheadline)
        .foregroundStyle(Theme.text)
    }

    private var footer: some View {
        HStack {
            Button("打开对话") { open("chat") }
            Button("设置") { open("settings") }
            Spacer()
            Menu {
                Button("立即整理汇报") { store.briefNow() }
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
