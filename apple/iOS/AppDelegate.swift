import LoopMeKit
import SwiftUI
import UIKit
import UserNotifications

@MainActor
final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    let store = LoopMeStore(saveCredentials: { KeychainCredentials.save($0) })

    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        let approve = UNNotificationAction(identifier: "APPROVE", title: "同意", options: [.authenticationRequired])
        let reject = UNNotificationAction(identifier: "REJECT", title: "拒绝", options: [.destructive])
        center.setNotificationCategories([
            UNNotificationCategory(identifier: "LOOPME_DECISION", actions: [approve, reject], intentIdentifiers: [], options: []),
        ])
        if let creds = KeychainCredentials.load() {
            store.start(with: creds)
            Push.requestAuthorization()
        }
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        let token = deviceToken.map { String(format: "%02x", $0) }.joined()
        store.registerPush(token: token, environment: Push.environment)
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        print("LoopMe: push registration failed: \(error)")
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            willPresent notification: UNNotification,
                                            withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .list, .sound])
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            didReceive response: UNNotificationResponse,
                                            withCompletionHandler completionHandler: @escaping () -> Void) {
        let action = response.actionIdentifier
        let decisionId = response.notification.request.content.userInfo["decisionId"] as? String
        Task { @MainActor in
            if let decisionId, action == "APPROVE" || action == "REJECT" {
                if self.store.credentials == nil, let creds = KeychainCredentials.load() {
                    self.store.start(with: creds)
                }
                _ = await self.store.answerInBackground(decisionId: decisionId, approve: action == "APPROVE")
            }
            completionHandler()
        }
    }
}

enum Push {
    #if DEBUG
    static let environment = "sandbox"
    #else
    static let environment = "production"
    #endif

    @MainActor
    static func requestAuthorization() {
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { granted, _ in
            guard granted else { return }
            DispatchQueue.main.async { UIApplication.shared.registerForRemoteNotifications() }
        }
    }
}

/// The paired Mac lives in the Keychain (readable after first unlock, so notification actions work).
enum KeychainCredentials {
    private static let service = "LoopMe.pairedMac"
    private static let account = "default"

    static func load() -> MacCredentials? {
        let q: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var out: CFTypeRef?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess, let data = out as? Data else { return nil }
        return try? JSONDecoder().decode(MacCredentials.self, from: data)
    }

    static func save(_ creds: MacCredentials?) {
        let base: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(base as CFDictionary)
        guard let creds, let data = try? JSONEncoder().encode(creds) else { return }
        var add = base
        add[kSecValueData as String] = data
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(add as CFDictionary, nil)
    }
}
