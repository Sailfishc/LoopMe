import LoopMeKit
import SwiftUI

@main
struct LoopMeMacApp: App {
    @NSApplicationDelegateAdaptor(MacAppDelegate.self) private var delegate
    @AppStorage(AppearanceSetting.key) private var appearance = "dark"

    var body: some Scene {
        MenuBarExtra {
            MacPanelView()
                .environmentObject(delegate.store)
                .environmentObject(delegate.daemon)
                .environmentObject(delegate.keepAwake)
                .preferredColorScheme(AppearanceSetting.colorScheme(appearance))
        } label: {
            MenuBarIcon().environmentObject(delegate.store)
        }
        .menuBarExtraStyle(.window)

        Window("LoopMe 对话", id: "chat") {
            ChatView()
                .environmentObject(delegate.store)
                .frame(minWidth: 420, minHeight: 560)
                .preferredColorScheme(AppearanceSetting.colorScheme(appearance))
        }
        .defaultSize(width: 480, height: 720)

        Window("LoopMe 设置", id: "settings") {
            NavigationStack {
                SettingsForm(isMac: true)
            }
            .environmentObject(delegate.store)
            .frame(minWidth: 460, minHeight: 600)
            .preferredColorScheme(AppearanceSetting.colorScheme(appearance))
        }
        .defaultSize(width: 520, height: 720)
    }
}

/// Menu bar: an icon, plus a short word while something is going on (等你决定 / 正在做).
struct MenuBarIcon: View {
    @EnvironmentObject var store: LoopMeStore
    var body: some View {
        let text = store.nowSummary?.menuBarText ?? ""
        HStack(spacing: 4) {
            Image(systemName: store.pendingDecisions.isEmpty ? (store.isWorking ? "circle.dotted.circle" : "circle.circle") : "exclamationmark.circle.fill")
            if !text.isEmpty { Text(text) }
        }
    }
}

@MainActor
final class MacAppDelegate: NSObject, NSApplicationDelegate {
    let store = LoopMeStore()
    let daemon = DaemonController()
    let keepAwake = KeepAwake()

    func applicationDidFinishLaunching(_ notification: Notification) {
        daemon.start(store: store)
        keepAwake.start()
    }

    func applicationWillTerminate(_ notification: Notification) {
        store.stopConnection()
        daemon.stop()
    }
}
