import LoopMeKit
import SwiftUI
import UserNotifications

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
                .environmentObject(delegate.updater)
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
            Image(systemName: store.waitingCount == 0 ? (store.isWorking ? "circle.dotted.circle" : "circle.circle") : "exclamationmark.circle.fill")
            if !text.isEmpty { Text(text) }
        }
    }
}

@MainActor
final class MacAppDelegate: NSObject, NSApplicationDelegate {
    let store = LoopMeStore()
    let daemon = DaemonController()
    let keepAwake = KeepAwake()
    let updater = AppUpdater()

    func applicationDidFinishLaunching(_ notification: Notification) {
        daemon.start(store: store)
        keepAwake.start()
        updater.start(daemon: daemon)
        setUpBanners()
    }

    /// A Codex session needs the user while they are at the Mac (but not looking at Codex): a system banner.
    private func setUpBanners() {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        center.requestAuthorization(options: [.alert, .sound]) { _, _ in }
        store.onAlert = { title, body, sessionId in
            let content = UNMutableNotificationContent()
            content.title = title
            content.body = body
            content.sound = .default
            content.threadIdentifier = "loopme-sessions"
            content.userInfo = ["sessionId": sessionId]
            // One banner per session: a newer one replaces the old.
            let id = sessionId.isEmpty ? UUID().uuidString : "session-\(sessionId)"
            center.add(UNNotificationRequest(identifier: id, content: content, trigger: nil))
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        store.stopConnection()
        daemon.stop()
    }
}

extension MacAppDelegate: UNUserNotificationCenterDelegate {
    /// LoopMe is a menu bar app, so it is always "in front": show the banner anyway.
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                                            withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .sound])
    }
}
