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
    /// The user's own Codex sessions (via Codex hooks), by id.
    @Published public private(set) var sessions: [String: CodexSession] = [:]
    @Published public private(set) var pairing = PairingState()
    @Published public private(set) var connection: LoopMeConnection.State = .idle
    @Published public private(set) var credentials: MacCredentials?
    /// Messages sent but not yet confirmed by the Mac.
    @Published public private(set) var outgoing: [OutgoingMessage] = []
    /// Decision id → choice, while the answer is in flight.
    @Published public private(set) var answering: [String: String] = [:]
    @Published public var lastError: String?
    @Published public private(set) var hasSynced = false
    /// The message the next chat message replies to (long press → 回复).
    @Published public var replyTo: Message?
    /// Offer message id → action, while the tap is in flight.
    @Published public private(set) var acting: [String: String] = [:]
    /// Which open suggestion is shown (suggestions come one at a time).
    @Published public var suggestionIndex = 0

    public struct OutgoingMessage: Identifiable, Equatable {
        public let id: String
        public let text: String
        public let createdAt: Date
        public var replyTo: String? = nil
        /// The Mac never got it (the link dropped). Tapping the bubble sends it again.
        public var failed = false
    }

    /// Server message id → the client id of the bubble it replaced, so the row keeps its identity (no jump).
    @Published public private(set) var rowKeys: [String: String] = [:]

    /// (title, body, session id) for a Mac banner; only the Mac app sets it.
    public var onAlert: ((String, String, String) -> Void)?

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
                appOpened()
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
        sessions = [:]
        outgoing = []
        rowKeys = [:]
        replyTo = nil
        hasSynced = false
        lastError = nil
        saveCredentials?(nil)
    }

    public func sync() async {
        do {
            let snap = try await conn.call("sync", as: Snapshot.self)
            messages = snap.messages.sorted(by: Self.order)
            let delivered = Set(messages.compactMap { rowKeys[$0.id] })
            outgoing.removeAll { delivered.contains($0.id) }
            items = Dictionary(snap.items.map { ($0.id, $0) }, uniquingKeysWith: { _, b in b })
            decisions = Dictionary(snap.decisions.map { ($0.id, $0) }, uniquingKeysWith: { _, b in b })
            memory = snap.memory
            profile = snap.profile
            settings = snap.settings
            status = snap.status
            devices = snap.devices
            sessions = Dictionary(snap.sessions.map { ($0.id, $0) }, uniquingKeysWith: { _, b in b })
            hasSynced = true
            if !conn.isConnected { return }
            if let c = credentials, c.macName != snap.status.macName, !snap.status.macName.isEmpty {
                var c2 = c
                c2.macName = snap.status.macName
                credentials = c2
                saveCredentials?(c2)
            }
        } catch {
            report(error)
        }
    }

    /// Shows an error alert, except for a dropped link: the connection banner already says that.
    private func report(_ error: Error) {
        if error is ConnectionError { return }
        lastError = error.localizedDescription
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
                if messages[i].offer != m.offer { acting[m.id] = nil }
                messages[i] = m
            } else {
                messages.append(m)
                messages.sort(by: Self.order)
            }
            if m.role == "user" {
                // The Mac has it now; the real message takes the pending bubble's place and identity.
                if rowKeys[m.id] == nil, let i = outgoing.firstIndex(where: { $0.text == m.text && !$0.failed }) {
                    rowKeys[m.id] = outgoing[i].id
                    outgoing.remove(at: i)
                }
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
        case "session":
            if let s = decode(CodexSession.self, data) { sessions[s.id] = s }
        case "alert":
            // A session needs the user while they are at the Mac: the Mac app shows a banner.
            if let a = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
               let title = a["title"] as? String, let body = a["body"] as? String {
                onAlert?(title, body, a["sessionId"] as? String ?? "")
            }
        case "pairing":
            if let p = decode(PairingState.self, data) { pairing = p }
        case "unpaired":
            forget()
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

    /// Suggested items waiting for the user, oldest first. Shown one at a time.
    public var openSuggestions: [Item] {
        items.values.filter { $0.status == "suggested" }.sorted { $0.createdAt < $1.createdAt }
    }

    public var currentSuggestion: Item? {
        let s = openSuggestions
        return s.isEmpty ? nil : s[suggestionIndex % s.count]
    }

    public func nextSuggestion() {
        suggestionIndex += 1
    }

    /// The user's sessions LoopMe is watching right now (newest first).
    public var watchedSessions: [CodexSession] {
        let since = nowMillis() - 24 * 3600 * 1000
        return sessions.values
            .filter { $0.state != "ended" && $0.updatedAt >= since && !$0.muted }
            .sorted { $0.updatedAt > $1.updatedAt }
    }

    /// Sessions with a card for the "现在" bar (not snoozed, not muted, no automation), newest first.
    public var attentionSessions: [CodexSession] {
        let now = nowMillis()
        return sessions.values
            .filter { s in
                guard let a = s.attention, !s.muted, !s.isNoise else { return false }
                return (a.snoozedUntil ?? 0) <= now
            }
            .sorted { ($0.attention?.at ?? 0) > ($1.attention?.at ?? 0) }
    }

    /// Sessions that need the user. An approval already shown as a decision on the phone isn't counted twice.
    public var needsYouSessions: [CodexSession] {
        let decided = pendingDecisions.contains { $0.kind == "sessionApproval" }
        return attentionSessions.filter { s in
            guard let a = s.attention, a.needsYou else { return false }
            return !(decided && a.kind == "approval")
        }
    }

    /// Sessions with a result (done, milestone).
    public var resultSessions: [CodexSession] {
        attentionSessions.filter { $0.attention?.needsYou == false }
    }

    /// The user's Codex sessions running right now.
    public var runningSessions: [CodexSession] {
        let since = nowMillis() - 24 * 3600 * 1000
        return sessions.values
            .filter { $0.state == "working" && $0.updatedAt >= since && !$0.muted && !$0.isNoise }
            .sorted { ($0.turnStartedAt ?? $0.updatedAt) < ($1.turnStartedAt ?? $1.updatedAt) }
    }

    /// 好的 / 知道了.
    public func ackSession(_ s: CodexSession) {
        Task { _ = await run("session.ack", ["sessionId": s.id]) }
    }

    /// 稍后: back in 30 minutes.
    public func snoozeSession(_ s: CodexSession) {
        Task { _ = await run("session.snooze", ["sessionId": s.id]) }
    }

    /// 这个会话先别提醒.
    public func muteSession(_ s: CodexSession) {
        Task { _ = await run("session.mute", ["sessionId": s.id]) }
    }

    public func message(_ id: String?) -> Message? {
        guard let id else { return nil }
        return messages.first { $0.id == id }
    }

    // MARK: actions

    private func run(_ method: String, _ params: [String: Any] = [:]) async -> Data? {
        do {
            return try await conn.call(method, params)
        } catch {
            report(error)
            return nil
        }
    }

    /// Sends a chat message. Returns false (keep the draft) when the Mac is unreachable.
    @discardableResult
    public func send(_ text: String) -> Bool {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty, isConnected else { return false }
        let clientId = UUID().uuidString
        var params: [String: Any] = ["clientId": clientId, "text": t]
        let quoted = replyTo?.id
        if let quoted { params["replyTo"] = quoted }
        outgoing.append(OutgoingMessage(id: clientId, text: t, createdAt: Date(), replyTo: quoted))
        replyTo = nil
        deliver(clientId, params)
        return true
    }

    private func deliver(_ clientId: String, _ params: [String: Any]) {
        Task {
            do {
                let data = try await conn.call("message.send", params)
                let id = (try? JSONSerialization.jsonObject(with: data) as? [String: Any])?["messageId"] as? String
                if let id, rowKeys[id] == nil { rowKeys[id] = clientId }
                // Normally the message event came first and already swapped the bubble; otherwise the next sync does.
                if let id, message(id) != nil { outgoing.removeAll { $0.id == clientId } }
            } catch {
                // Keep the bubble so nothing typed is lost; a tap retries with the same client id (the Mac de-duplicates).
                if let i = outgoing.firstIndex(where: { $0.id == clientId }) { outgoing[i].failed = true }
                report(error)
            }
        }
    }

    /// Sends a message that failed to reach the Mac again.
    public func resend(_ clientId: String) {
        guard isConnected, let i = outgoing.firstIndex(where: { $0.id == clientId }), outgoing[i].failed else { return }
        outgoing[i].failed = false
        let o = outgoing[i]
        var params: [String: Any] = ["clientId": o.id, "text": o.text]
        if let r = o.replyTo { params["replyTo"] = r }
        deliver(o.id, params)
    }

    /// Drops a message that failed to send.
    public func discardOutgoing(_ clientId: String) {
        outgoing.removeAll { $0.id == clientId }
    }

    /// 👍 / 👎 on an agent message; the same value again clears it.
    public func feedback(_ message: Message, _ value: String) {
        let v = message.feedback == value ? "" : value
        Task { _ = await run("message.feedback", ["messageId": message.id, "value": v]) }
    }

    /// A tap on an offer card's button (让我来, 我自己来, 推送, 撤销 …).
    public func offerAct(_ message: Message, _ action: String) {
        guard isConnected, acting[message.id] == nil else { return }
        acting[message.id] = action
        Task {
            if await run("offer.act", ["messageId": message.id, "action": action, "clientId": UUID().uuidString]) == nil {
                acting[message.id] = nil
            }
        }
    }

    /// The agent's changes behind a result card (看改动).
    public func offerDiff(_ message: Message) async -> String {
        do {
            let d = try await conn.call("offer.diff", ["messageId": message.id])
            return (try? JSONDecoder().decode(DiffReply.self, from: d).diff) ?? ""
        } catch {
            return error.localizedDescription
        }
    }

    /// The app is in front of the user: the first time today starts the brief.
    public func appOpened() {
        guard isConnected else { return }
        Task { _ = await run("app.opened") }
    }

    /// The Mac's one-time confirm: install LoopMe's Codex hooks and trust them.
    public func enableCodexHooks() {
        Task { _ = await run("hooks.enable") }
    }

    public func disableCodexHooks() {
        Task { _ = await run("hooks.disable") }
    }

    public func stopRun() {
        Task { _ = await run("run.stop") }
    }

    public func answer(_ decision: Decision, approve: Bool) {
        answer(decision, choice: approve ? "approve" : "reject")
    }

    /// `approve`, `reject`, or `later` (回 Mac 再说: Codex asks again on the Mac).
    public func answer(_ decision: Decision, choice: String) {
        guard isConnected, answering[decision.id] == nil else { return }
        answering[decision.id] = choice
        Task {
            if await run("decision.answer", ["decisionId": decision.id, "choice": choice, "clientId": UUID().uuidString]) == nil {
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
        // Revocation can close this connection before the RPC response arrives.
        if isConnected { _ = try? await conn.call("device.unpair") }
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

private struct DiffReply: Decodable {
    let diff: String
}
