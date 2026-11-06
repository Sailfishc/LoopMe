import Combine
import Foundation
import Sparkle

/// Sparkle auto-update. Checks the appcast at `SUFeedURL` once a day, downloads
/// in the background and asks before installing. Without a feed URL (local
/// builds) the updater stays off.
@MainActor
final class AppUpdater: NSObject, ObservableObject, SPUUpdaterDelegate, SPUStandardUserDriverDelegate {
    @Published private(set) var canCheck = false
    private var controller: SPUStandardUpdaterController?
    private weak var daemon: DaemonController?

    /// Configured when the build set `LOOPME_UPDATE_FEED_URL` (see docs/mac-updates.md).
    static var feedURL: String? {
        (Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") as? String).flatMap { $0.hasPrefix("http") ? $0 : nil }
    }

    var enabled: Bool { controller != nil }

    func start(daemon: DaemonController) {
        self.daemon = daemon
        guard Self.feedURL != nil else { return }
        let c = SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: self, userDriverDelegate: self)
        controller = c
        c.updater.publisher(for: \.canCheckForUpdates).receive(on: RunLoop.main).assign(to: &$canCheck)
    }

    func checkForUpdates() {
        controller?.checkForUpdates(nil)
    }

    // The new app brings a new daemon: stop the running one before the bundle is replaced.
    nonisolated func updater(_ updater: SPUUpdater, willInstallUpdate item: SUAppcastItem) {
        Task { @MainActor in self.daemon?.stop() }
    }

    // A menu bar app has no Dock icon to badge; let Sparkle show its reminders gently.
    nonisolated var supportsGentleScheduledUpdateReminders: Bool { true }
}
