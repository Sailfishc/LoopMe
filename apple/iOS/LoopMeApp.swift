import LoopMeKit
import SwiftUI

@main
struct LoopMeApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @AppStorage(AppearanceSetting.key) private var appearance = "dark"
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(delegate.store)
                .preferredColorScheme(AppearanceSetting.colorScheme(appearance))
                .tint(Theme.text)
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .active:
                delegate.store.conn.reconnectNow()
            case .background:
                // Locked or switched away: iOS freezes the socket, so close it cleanly and reconnect on return.
                delegate.store.conn.suspend()
            default:
                break
            }
        }
    }
}
