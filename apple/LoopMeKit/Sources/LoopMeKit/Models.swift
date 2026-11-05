import Foundation

// Mirrors `daemon/src/model.rs`. JSON keys are camelCase; times are Unix milliseconds.

public struct Message: Codable, Identifiable, Equatable, Hashable {
    public var id: String
    public var seq: Int64
    /// `user`, `agent` or `system`.
    public var role: String
    public var text: String
    public var itemId: String?
    public var decisionId: String?
    public var createdAt: Int64
    public var streaming: Bool
}

public struct Item: Codable, Identifiable, Equatable, Hashable {
    public var id: String
    public var title: String
    public var detail: String
    /// `scheduled`, `active`, `needsYou`, `snoozed`, `done`, `dropped`, `suggested`.
    public var status: String
    public var nextCheckAt: Int64?
    public var doneWhen: String?
    public var source: String?
    public var checks: Int64
    public var unchangedChecks: Int64
    public var createdAt: Int64
    public var updatedAt: Int64

    public var isOpen: Bool { status != "done" && status != "dropped" }
}

public struct Decision: Codable, Identifiable, Equatable, Hashable {
    public var id: String
    public var itemId: String?
    public var title: String
    public var detail: String
    /// `approval`, `question` or `continue`.
    public var kind: String
    /// `pending`, `approved`, `rejected`, `expired`.
    public var status: String
    public var createdAt: Int64
    public var expiresAt: Int64
    public var answeredAt: Int64?
}

public struct MemoryEntry: Codable, Identifiable, Equatable, Hashable {
    public var id: String
    public var text: String
    /// `agent` or `user`.
    public var author: String
    /// `active`, `locked`, `deleted`.
    public var status: String
    public var createdAt: Int64
    public var updatedAt: Int64
}

public struct Profile: Codable, Equatable {
    public var name: String = ""
    public var timezone: String = ""
    public var workHours: String = ""
    public var language: String = ""
    public var about: String = ""
    public init() {}
}

public struct NotifySettings: Codable, Equatable {
    public var decision: Bool = true
    public var result: Bool = true
    public var brief: Bool = true
    public var failure: Bool = true
    public init() {}
}

public struct Limits: Codable, Equatable {
    public var perItemPerDay: Int64 = 2
    public var perHour: Int64 = 5
    public var decisionTimeoutHours: Int64 = 48
    public var maxUnchangedChecks: Int64 = 10
    public init() {}
}

public struct Settings: Codable, Equatable {
    /// `readOnly`, `workspace` or `full`.
    public var permissionLevel: String = "workspace"
    public var workspaceRoots: [String] = []
    public var briefEnabled: Bool = true
    public var briefTime: String = "09:00"
    public var briefWeekdaysOnly: Bool = true
    public var quietStart: String = "22:00"
    public var quietEnd: String = "08:00"
    public var paused: Bool = false
    public var pausedUntil: Int64?
    public var notify = NotifySettings()
    public var showSteps: Bool = false
    public var lockScreenSummary: Bool = false
    public var sessionAnalysis: Bool = true
    public var codexPath: String?
    public var model: String?
    public var limits = Limits()
    public init() {}
}

public struct CodexStatus: Codable, Equatable {
    public var found: Bool = false
    public var path: String?
    public var version: String?
    public var loggedIn: Bool?
    public var error: String?
    public init() {}
}

public struct RunInfo: Codable, Equatable {
    public var id: String
    public var title: String
    public var startedAt: Int64
}

public struct Status: Codable, Equatable {
    public var macName: String = ""
    public var daemonVersion: String = ""
    public var codex = CodexStatus()
    /// `idle`, `working`, `paused`, `error`.
    public var agent: String = "idle"
    public var currentRun: RunInfo?
    public var nextWakeAt: Int64?
    public var pushConfigured: Bool = false
    public var listenPort: Int = 7788
    public var hosts: [String] = []
    public init() {}
}

public struct Device: Codable, Identifiable, Equatable, Hashable {
    public var id: String
    public var name: String
    public var createdAt: Int64
    public var lastSeenAt: Int64?
    public var isLocal: Bool
    public var hasPush: Bool
}

public struct Snapshot: Codable {
    public var seq: Int64
    public var messages: [Message]
    public var items: [Item]
    public var decisions: [Decision]
    public var memory: [MemoryEntry]
    public var profile: Profile
    public var settings: Settings
    public var status: Status
    public var devices: [Device]
}

public struct PairingState: Codable, Equatable {
    /// `idle`, `waiting`, `confirm`, `paired`, `failed`.
    public var state: String = "idle"
    public var url: String?
    public var sas: String?
    public var deviceName: String?
    public var expiresAt: Int64?
    public var error: String?
    public init() {}
}

public extension Int64 {
    var date: Date { Date(timeIntervalSince1970: TimeInterval(self) / 1000) }
}

public func nowMillis() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }
