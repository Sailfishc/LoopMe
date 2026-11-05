import LoopMeKit
import SwiftUI
import UIKit

/// First run: scan the QR code shown on the Mac, compare the 4-digit code, done.
struct ConnectView: View {
    @EnvironmentObject var store: LoopMeStore
    @Binding var incomingLink: String?
    @State private var scanning = false
    @State private var phase: Phase = .start

    enum Phase: Equatable {
        case start
        case connecting(String)
        case confirm(String)
        case failed(String)
    }

    var body: some View {
        VStack(spacing: 0) {
            Spacer()
            Image(systemName: "laptopcomputer.and.iphone")
                .font(.system(size: 56, weight: .light))
                .foregroundStyle(Theme.text)
                .padding(.bottom, 28)
            Text("连接你的 Mac")
                .font(.title.weight(.semibold))
                .foregroundStyle(Theme.text)
            content
                .padding(.top, 14)
                .padding(.horizontal, 32)
            Spacer()
            buttons
                .padding(.horizontal, 24)
                .padding(.bottom, 32)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background.ignoresSafeArea())
        .sheet(isPresented: $scanning) {
            ScannerSheet { code in
                scanning = false
                startPairing(code)
            }
        }
        .onChange(of: incomingLink) { _, link in
            if let link {
                incomingLink = nil
                startPairing(link)
            }
        }
        .onAppear {
            if let link = incomingLink {
                incomingLink = nil
                startPairing(link)
            }
        }
    }

    @ViewBuilder private var content: some View {
        switch phase {
        case .start:
            VStack(alignment: .leading, spacing: 12) {
                step(1, "在 Mac 菜单栏打开 LoopMe，点“连接手机”。")
                step(2, "用这里的“扫码连接”扫描 Mac 上的二维码。")
                step(3, "核对两边显示的 4 位校验码，在 Mac 上确认。")
                Text("手机需要和 Mac 在同一网络，或已连上公司 VPN。")
                    .font(.footnote).foregroundStyle(Theme.secondary).padding(.top, 6)
            }
        case .connecting(let name):
            VStack(spacing: 12) {
                ProgressView()
                Text("正在连接 \(name)…").foregroundStyle(Theme.secondary)
            }
        case .confirm(let sas):
            VStack(spacing: 14) {
                Text("校验码").font(.subheadline).foregroundStyle(Theme.secondary)
                Text(sas)
                    .font(.system(size: 52, weight: .semibold, design: .rounded).monospacedDigit())
                    .tracking(10)
                    .foregroundStyle(Theme.text)
                Text("请确认 Mac 上显示的是同样的 4 位数字，并在 Mac 上点“确认配对”。")
                    .font(.footnote).foregroundStyle(Theme.secondary).multilineTextAlignment(.center)
                ProgressView().padding(.top, 4)
            }
        case .failed(let msg):
            VStack(spacing: 10) {
                Image(systemName: "exclamationmark.triangle").foregroundStyle(Theme.warning)
                Text(msg).font(.footnote).foregroundStyle(Theme.secondary).multilineTextAlignment(.center)
            }
        }
    }

    @ViewBuilder private var buttons: some View {
        switch phase {
        case .connecting, .confirm:
            Button("取消") { phase = .start }
                .buttonStyle(PillButtonStyle(prominent: false))
        default:
            VStack(spacing: 12) {
                Button {
                    scanning = true
                } label: {
                    Text("扫码连接").frame(maxWidth: .infinity)
                }
                .buttonStyle(PillButtonStyle())
                Button {
                    if let s = UIPasteboard.general.string { startPairing(s) }
                } label: {
                    Text("粘贴配对链接").frame(maxWidth: .infinity)
                }
                .buttonStyle(PillButtonStyle(prominent: false))
            }
        }
    }

    private func step(_ n: Int, _ text: String) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Text("\(n)")
                .font(.caption.weight(.bold))
                .frame(width: 22, height: 22)
                .foregroundStyle(Theme.pillText)
                .background(Theme.pill, in: Circle())
            Text(text).font(.subheadline).foregroundStyle(Theme.text)
        }
    }

    @MainActor
    private func startPairing(_ s: String) {
        guard let link = PairingLink(string: s) else {
            phase = .failed("这不是 LoopMe 的配对二维码。")
            return
        }
        phase = .connecting(link.macName)
        Task { @MainActor in
            do {
                let creds = try await LoopMePairing.pair(link: link, deviceName: UIDevice.current.name) { sas in
                    Task { @MainActor in phase = .confirm(sas) }
                }
                guard phase != .start else { return } // cancelled
                KeychainCredentials.save(creds)
                store.start(with: creds)
                Push.requestAuthorization()
                phase = .start
            } catch {
                if phase != .start { phase = .failed(error.localizedDescription) }
            }
        }
    }
}

private struct ScannerSheet: View {
    let onCode: (String) -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ZStack(alignment: .top) {
            QRScannerView(onCode: onCode).ignoresSafeArea()
            HStack {
                Text("扫描 Mac 上的二维码").font(.headline).foregroundStyle(.white)
                Spacer()
                Button("关闭") { dismiss() }.foregroundStyle(.white)
            }
            .padding()
            .background(.black.opacity(0.4))
        }
    }
}
