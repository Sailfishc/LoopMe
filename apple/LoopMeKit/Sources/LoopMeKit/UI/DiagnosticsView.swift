import SwiftUI

/// 设置 → 连接诊断: checks every address of the paired Mac and says in one line why the phone can't reach it.
public struct ConnectionDiagnosticsView: View {
    @EnvironmentObject var store: LoopMeStore
    @State private var report: ConnectionReport?
    @State private var running = false
    @State private var copied = false

    public init() {}

    public var body: some View {
        List {
            Section {
                if let report {
                    Text(report.verdict)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(report.verdict == "连接正常" ? Theme.green : Theme.warning)
                } else {
                    Text("会逐个检查配对时记下的 Mac 地址：网络能不能到达、Relay 是否响应、Mac 是否在线。")
                        .font(.subheadline).foregroundStyle(Theme.secondary)
                }
                Button(running ? "正在检查…" : (report == nil ? "开始诊断" : "再查一次")) { Task { await run() } }
                    .disabled(running || store.credentials == nil)
            }
            if let report {
                Section("地址") {
                    ForEach(report.rows) { r in
                        VStack(alignment: .leading, spacing: 4) {
                            Text("\(r.host):\(report.port)").font(.subheadline.monospaced()).foregroundStyle(Theme.text)
                            HStack(spacing: 10) {
                                step("网络", r.tcp, r.tcpOK)
                                if let relay = r.relay { step("Relay", relay, r.relayOK) }
                                if let m = r.macOnline { step("Mac", m ? "在线" : "不在线", m) }
                            }
                        }
                        .padding(.vertical, 2)
                    }
                }
                Section("这台手机") {
                    Text(report.onVPN ? "检测到 VPN 网卡" : "没有检测到 VPN 网卡").foregroundStyle(Theme.text)
                    ForEach(report.phoneAddresses, id: \.self) { a in
                        Text(a).font(.caption.monospaced()).foregroundStyle(Theme.secondary)
                    }
                }
                Section {
                    Button(copied ? "已复制" : "复制报告") {
                        Clipboard.copy(report.text)
                        copied = true
                    }
                } footer: {
                    Text("发给 IT 或 LoopMe 维护者时附上这份报告。里面没有聊天内容和密钥。")
                }
            }
        }
        .navigationTitle("连接诊断")
        .task { if report == nil { await run() } }
    }

    private func step(_ name: String, _ value: String, _ ok: Bool) -> some View {
        HStack(spacing: 4) {
            Image(systemName: ok ? "checkmark.circle.fill" : "xmark.circle.fill")
                .foregroundStyle(ok ? Theme.green : Theme.danger)
            Text("\(name) \(value)").lineLimit(1)
        }
        .font(.caption)
        .foregroundStyle(Theme.secondary)
    }

    private func run() async {
        guard let creds = store.credentials, !running else { return }
        running = true
        copied = false
        report = await LoopMeDiagnostics.run(creds)
        running = false
    }
}
