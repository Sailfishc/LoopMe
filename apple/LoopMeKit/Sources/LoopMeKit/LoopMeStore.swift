import Foundation
import SwiftUI

/// App state for one paired Mac. Only state confirmed by the Mac counts:
/// actions show “已发送” until the Mac's event arrives.
@MainActor
public final class LoopMeStore: ObservableObject {
    @Published public private(set) var messages: [Message] = []
    @Published public private(set) var items: [String: Item] = [:]
    @Published public private(set) var decisions: [String: Decision] = [:]
    @Published public private(set) var memory: [MemoryEntry] = []
    @Published public private(set) var profile = Profile()
    @Published public private(set) var settings = Settings()
    @Published public private(set) var status = Status()
    @Published public private(set) var devices: [Device] = []
    @Published public private(set) var pairing = PairingState()
    @Published public private(set) var connection: LoopMeConnection.State = .idle
    @Published public private(set) var credentials: MacCredentials?
    /// Messages sent but not yet confirmed by the Mac.
    @Published public private(set) var outgoing: [OutgoingMessage] = []
    /// Decision id → choice, while the answer is in flight.
    @Published public private(set) var answering: [String: String] = [:]
    @Published public var lastError: String?
    @Published public private(set) var hasSynced = false

    public struct OutgoingMessage: Identifiable, Equatable {
        public let id: String
        public let text: String
        public let createdAt: Date
    }

    public let conn = LoopMeConnection()
    /// APNs token waiting to be sent to the Mac (sent on every connect).
    private var pushToken: (token: String, env: String)?
    private let saveCredentials: ((MacCredentials?) -> Void)?

    /// `saveCredentials` persists changes (e.g. the host that worked, or unpairing).
    public init(saveCredentials: ((MacCredentials?) -> Void)? = nil) {
        self.saveCredentials = saveCredentials
        conn.onState = { [weak self] s in self?.connectionChanged(s) }
        conn.onEvent = { [weak self] name, seq, data in self?.apply(event: name, seq: seq, data: data) }
        conn.onUnpaired = { [weak self] in self?.forget() }
        conn.onConnectedHost = { [weak self] host in self?.preferHost(host) }
    }

    // MARK: lifecycle

    public func start(with creds: MacCredentials) {
        credentials = creds
        conn.connect(creds)
    }

    public func stopConnection() {
        conn.disconnect()
    }

    public var isConnected: Bool { connection == .connected }

    private func connectionChanged(_ s: LoopMeConnection.State) {
        connection = s
        if s == .connected {
            Task {
                await sync()
                if let p = pushToken { _ = await run("push.register", ["token": p.token, "env": p.env]) }
            }
        } else {
            answering = [:]
        }
    }

    private func preferHost(_ host: String) {
        guard var c = credentials, c.hosts.first != host else { return }
        c.hosts.removeAll { $0 == host }
        c.hosts.insert(host, at: 0)
        credentials = c
        saveCredentials?(c)
    }

    /// Clears the pairing locally (after the Mac revoked us, or when unpairing).
    public func forget() {
        conn.disconnect()
        credentials = nil
        messages = []
        items = [:]
        decisions = [:]
        memory = []
        devices = []
        outgoing = []
        hasSynced = false
        saveCredentials?(nil)
    }

    public func sync() async {
        do {
            let snap = try await conn.call("sync", as: Snapshot.self)
            messages = snap.messages.sorted(by: Self.order)
            items = Dictionary(snap.items.map { ($0.id, $0) }, uniquingKeysWith: { _, b in b })
            decisions = Dictionary(snap.decisions.map { ($0.id, $0) }, uniquingKeysWith: { _, b in b })
            memory = snap.memory
            profile = snap.profile
            settings = snap.settings
            status = snap.status
            devices = snap.devices
            hasSynced = true
            if !conn.isConnected { return }
            if let c = credentials, c.macName != snap.status.macName, !snap.status.macName.isEmpty {
                var c2 = c
                c2.macName = snap.status.macName
                credentials = c2
                saveCredentials?(c2)
            }
        } catch {
            lastError = error.localizedDescription
        }
    }

    private static func order(_ a: Message, _ b: Message) -> Bool {
        a.createdAt == b.createdAt ? a.seq < b.seq : a.createdAt < b.createdAt
    }

    // MARK: events

    private func decode<T: Decodable>(_ type: T.Type, _ data: Data) -> T? {
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            print("LoopMe: cannot decode \(T.self): \(error)")
            return nil
        }
    }

    public func apply(event: String, seq: Int64, data: Data) {
        switch event {
        case "message":
            guard let m = decode(Message.self, data) else { return }
            if let i = messages.firstIndex(where: { $0.id == m.id }) {
                messages[i] = m
            } else {
                messages.append(m)
                messages.sort(by: Self.order)
            }
            if m.role == "user" {
                // The Mac has it now; drop the matching pending bubble.
                if let i = outgoing.firstIndex(where: { $0.text == m.text }) { outgoing.remove(at: i) }
            }
        case "messageDelta":
            guard let d = decode(MessageDelta.self, data), let i = messages.firstIndex(where: { $0.id == d.id }) else { return }
            messages[i].text = d.text
        case "item":
            if let i = decode(Item.self, data) { items[i.id] = i }
        case "decision":
            if let d = decode(Decision.self, data) {
                decisions[d.id] = d
                if d.status != "pending" { answering[d.id] = nil }
            }
        case "memory":
            if let m = decode([MemoryEntry].self, data) { memory = m }
        case "profile":
            if let p = decode(Profile.self, data) { profile = p }
        case "settings":
            if let s = decode(Settings.self, data) { settings = s }
        case "status":
            if let s = decode(Status.self, data) { status = s }
        case "devices":
            if let d = decode([Device].self, data) { devices = d }
        case "pairing":
            if let p = decode(PairingState.self, data) { pairing = p }
        case "resync":
            Task { await sync() }
        default:
            break
        }
    }

    // MARK: derived

    public var pendingDecisions: [Decision] {
        decisions.values.filter { $0.status == "pending" }.sorted { $0.createdAt < $1.createdAt }
    }

    public var openItems: [Item] {
        items.values.filter { $0.isOpen && $0.status != "suggested" }.sorted { $0.createdAt < $1.createdAt }
    }

    public var isWorking: Bool { status.agent == "working" }

    // MARK: actions

    private func run(_ method: String, _ params: [String: Any] = [:]) async -> Data? {
        do {
            return try await conn.call(method, params)
        } catch {
            lastError = error.localizedDescription
            return nil
        }
    }

    /// Sends a chat message. Returns false (keep the draft) when the Mac is unreachable.
    @discardableResult
    public func send(_ text: String) -> Bool {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty, isConnected else { return false }
        let clientId = UUID().uuidString
        outgoing.append(OutgoingMessage(id: clientId, text: t, createdAt: Date()))
        Task {
            if await run("message.send", ["clientId": clientId, "text": t]) == nil {
                outgoing.removeAll { $0.id == clientId }
            }
        }
        return true
    }

    public func stopRun() {
        Task { _ = await run("run.stop") }
    }

    public func answer(_ decision: Decision, approve: Bool) {
        guard isConnected, answering[decision.id] == nil else { return }
        answering[decision.id] = approve ? "approve" : "reject"
        Task {
            if await run("decision.answer", ["decisionId": decision.id, "choice": approve ? "approve" : "reject", "clientId": UUID().uuidString]) == nil {
                answering[decision.id] = nil
            }
        }
    }

    /// For notification actions: answer without showing UI. Waits for the connection briefly.
    public func answerInBackground(decisionId: String, approve: Bool) async -> Bool {
        for _ in 0..<40 where !isConnected {
            try? await Task.sleep(nanoseconds: 250_000_000)
        }
        guard isConnected else { return false }
        do {
            _ = try await conn.call("decision.answer", ["decisionId": decisionId, "choice": approve ? "approve" : "reject", "clientId": "notif-\(decisionId)"])
            return true
        } catch {
            return false
        }
    }

    public func itemAction(_ item: Item, _ action: String, minutes: Int? = nil) {
        var p: [String: Any] = ["itemId": item.id, "action": action]
        if let minutes { p["minutes"] = minutes }
        Task { _ = await run("item.action", p) }
    }

    public func addMemory(_ text: String, locked: Bool = false) {
        Task { _ = await run("memory.add", ["text": text, "locked": locked]) }
    }

    public func updateMemory(_ entry: MemoryEntry, text: String? = nil, locked: Bool? = nil) {
        var p: [String: Any] = ["id": entry.id]
        if let text { p["text"] = text }
        if let locked { p["locked"] = locked }
        Task { _ = await run("memory.update", p) }
    }

    public func deleteMemory(_ entry: MemoryEntry) {
        Task { _ = await run("memory.delete", ["id": entry.id]) }
    }

    public func restoreMemory(_ entry: MemoryEntry) {
        Task { _ = await run("memory.restore", ["id": entry.id]) }
    }

    public func updateProfile(_ patch: [String: Any]) {
        Task { _ = await run("profile.update", patch) }
    }

    public func updateSettings(_ patch: [String: Any]) {
        Task { _ = await run("settings.update", patch) }
    }

    /// Remembers the APNs token and sends it now (if connected) and after every reconnect.
    public func registerPush(token: String, environment: String) {
        pushToken = (token, environment)
        guard isConnected else { return }
        Task { _ = await run("push.register", ["token": token, "env": environment]) }
    }

    public func analyzeSessions() {
        Task { _ = await run("sessions.analyze") }
    }

    /// Removes this phone on the Mac, then forgets the pairing locally.
    public func unpair() async {
        if isConnected { _ = await run("device.unpair") }
        forget()
    }

    // Mac app only

    public func startPairing() {
        Task { _ = await run("pairing.start") }
    }

    public func cancelPairing() {
        Task { _ = await run("pairing.cancel") }
    }

    public func confirmPairing(_ accept: Bool) {
        Task { _ = await run("pairing.confirm", ["accept": accept]) }
    }

    public func refreshPairing() {
        Task {
            if let d = await run("pairing.state"), let p = try? JSONDecoder().decode(PairingState.self, from: d) {
                pairing = p
            }
        }
    }

    public func revoke(_ device: Device) {
        Task { _ = await run("devices.revoke", ["deviceId": device.id]) }
    }

    public func refreshStatus() {
        Task { _ = await run("status.refresh") }
    }

    public func codexLogin() {
        Task { _ = await run("codex.login") }
    }

    public func briefNow() {
        Task { _ = await run("brief.now") }
    }

    public func testPush() async -> String {
        do {
            let d = try await conn.call("push.test")
            return (try? JSONDecoder().decode(PushReport.self, from: d).report) ?? "已发送"
        } catch {
            return error.localizedDescription
        }
    }
}

private struct MessageDelta: Decodable {
    let id: String
    let text: String
}

private struct PushReport: Decodable {
    let report: String
}
