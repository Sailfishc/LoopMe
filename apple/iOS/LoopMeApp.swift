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
            if phase == .active {
                delegate.store.conn.reconnectNow()
            }
        }
    }
}
