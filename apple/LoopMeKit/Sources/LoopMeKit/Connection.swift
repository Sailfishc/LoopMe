import CryptoKit
import Foundation
import Network

public struct RPCError: LocalizedError {
    public var message: String
    public var errorDescription: String? { message }
}

/// The link to the Mac is down (not an answer from the Mac). The connection banner already says so,
/// so callers should not show it as an error alert.
public struct ConnectionError: LocalizedError {
    public var message: String
    public var errorDescription: String? { message }
}

enum HandshakeError: LocalizedError {
    case unknownDevice
    case server(String)
    case badHello

    var errorDescription: String? {
        switch self {
        case .unknownDevice: return "这台设备已在 Mac 上解除配对"
        case .server(let s): return s
        case .badHello: return "握手失败"
        }
    }
}

/// An encrypted WebSocket session with the daemon (see `daemon/src/server.rs`).
/// All state lives on the main actor; sends are serialized so frame counters stay in order.
@MainActor
public final class LoopMeConnection {
    public enum State: Equatable {
        case idle
        case connecting
        case connected
        case failed(String)
    }

    public private(set) var state: State = .idle {
        didSet { if state != oldValue { onState?(state) } }
    }
    public var onState: ((State) -> Void)?
    /// (event name, seq, JSON payload)
    public var onEvent: ((String, Int64, Data) -> Void)?
    public var onUnpaired: (() -> Void)?
    /// Called after a successful handshake with the host that worked.
    public var onConnectedHost: ((String) -> Void)?

    private var creds: MacCredentials?
    private var socket: URLSessionWebSocketTask?
    private var sealer: FrameSealer?
    private var opener: FrameOpener?
    private var pending: [String: CheckedContinuation<Data, Error>] = [:]
    private var receiveTask: Task<Void, Never>?
    private var pingTask: Task<Void, Never>?
    private var reconnectTask: Task<Void, Never>?
    private var lastSend: Task<Void, Never>?
    private var generation = 0
    private var wantConnected = false
    private var retryDelay: Double = 1
    /// Recreated per Mac: it pins that Mac's relay certificate.
    private var session: URLSession = LoopMeEndpoint.session(pinning: nil)
    private var sessionPin: String??
    private let pathMonitor = NWPathMonitor()
    private var lastPath: String?

    public init() {
        // Wi-Fi, cellular or the VPN came up or went away: try again now instead of waiting out the backoff.
        pathMonitor.pathUpdateHandler = { [weak self] path in
            let key = "\(path.status)|" + path.availableInterfaces.map(\.name).joined(separator: ",")
            Task { @MainActor in self?.pathChanged(key, satisfied: path.status == .satisfied) }
        }
        pathMonitor.start(queue: .global(qos: .utility))
    }

    private func pathChanged(_ key: String, satisfied: Bool) {
        defer { lastPath = key }
        guard lastPath != nil, key != lastPath, satisfied, wantConnected else { return }
        if state == .connected {
            // The old socket may be dead on the new route; a ping finds out within seconds.
            let gen = generation
            Task {
                do { _ = try await self.call("ping", timeout: 4) } catch { if gen == self.generation { self.connectionLost("网络变化，重新连接") } }
            }
        } else {
            retryDelay = 1
            startConnecting()
        }
    }

    public var isConnected: Bool { state == .connected }

    public func connect(_ creds: MacCredentials) {
        self.creds = creds
        wantConnected = true
        retryDelay = 1
        startConnecting()
    }

    public func disconnect() {
        wantConnected = false
        teardown("已断开")
        state = .idle
    }

    /// Reconnects right away (e.g. when the app comes to the foreground).
    /// A socket that looks connected may have died while the phone was locked, so it is checked first.
    public func reconnectNow() {
        guard wantConnected else { return }
        switch state {
        case .connecting:
            return
        case .connected:
            let gen = generation
            Task {
                do { _ = try await self.call("ping", timeout: 4) } catch {
                    if gen == self.generation { self.connectionLost("重新连接") }
                }
            }
        default:
            retryDelay = 1
            startConnecting()
        }
    }

    /// The app went to the background (e.g. the phone was locked). iOS freezes the socket soon after,
    /// so close it now and reconnect cleanly in `reconnectNow()` instead of finding a dead socket later.
    public func suspend() {
        guard wantConnected, state != .idle else { return }
        teardown("已暂停")
        state = .idle
    }

    // MARK: RPC

    public func call(_ method: String, _ params: [String: Any] = [:], timeout: Double = 30) async throws -> Data {
        guard state == .connected, let sealer else { throw ConnectionError(message: "未连接到 Mac") }
        let gen = generation
        let id = UUID().uuidString
        let body = try JSONSerialization.data(withJSONObject: ["id": id, "method": method, "params": params])
        let frame = try sealer.seal(body)
        return try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Data, Error>) in
            pending[id] = cont
            enqueueSend(frame) { [weak self] error in
                self?.pending.removeValue(forKey: id)?.resume(throwing: error)
            }
            Task { [weak self] in
                try? await Task.sleep(nanoseconds: UInt64(timeout * 1_000_000_000))
                guard let self, let c = self.pending.removeValue(forKey: id) else { return }
                let err: Error = method == "ping" ? ConnectionError(message: "Mac 没有响应") : RPCError(message: "Mac 没有及时响应，稍后再试")
                c.resume(throwing: err)
                // Maybe the socket is gone (e.g. it died while the phone was locked): check now instead of at the next ping.
                guard gen == self.generation, self.state == .connected else { return }
                if method == "ping" {
                    self.connectionLost("Mac 没有响应，重新连接")
                } else {
                    Task {
                        do { _ = try await self.call("ping", timeout: 4) } catch {}
                    }
                }
            }
        }
    }

    /// Calls and decodes the result.
    public func call<T: Decodable>(_ method: String, _ params: [String: Any] = [:], as type: T.Type, timeout: Double = 30) async throws -> T {
        let data = try await call(method, params, timeout: timeout)
        return try JSONDecoder().decode(T.self, from: data)
    }

    // MARK: internals

    private func enqueueSend(_ frame: Data, onError: @escaping (Error) -> Void) {
        let prev = lastSend
        let ws = socket
        lastSend = Task {
            await prev?.value
            do {
                guard let ws else { throw ConnectionError(message: "未连接到 Mac") }
                try await ws.send(.data(frame))
            } catch {
                onError(error is RPCError ? error : ConnectionError(message: error.localizedDescription))
            }
        }
    }

    private func startConnecting() {
        guard wantConnected, let creds else { return }
        teardown("重新连接")
        let gen = generation
        state = .connecting
        Task { await self.attempt(creds, gen) }
    }

    private struct Opened {
        let ws: URLSessionWebSocketTask
        let sealer: FrameSealer
        let opener: FrameOpener
    }

    private enum Outcome {
        case ok(String, Opened)
        case unknownDevice
        case failed(String)
    }

    /// Tries the addresses at the same time (each a little after the previous one) and keeps the first that completes the handshake.
    private func attempt(_ creds: MacCredentials, _ gen: Int) async {
        let pin: String? = creds.relay?.fingerprint
        if sessionPin != .some(pin) {
            session.finishTasksAndInvalidate()
            session = LoopMeEndpoint.session(pinning: pin)
            sessionPin = .some(pin)
        }
        let hosts = Array(creds.hosts.prefix(6))
        let outcome: Outcome = await withCheckedContinuation { cont in
            let race = Race(cont, count: hosts.count) { errors in Outcome.failed(errors.last ?? "无法连接到 Mac") }
            for (i, host) in hosts.enumerated() {
                Task { @MainActor in
                    if i > 0 { try? await Task.sleep(nanoseconds: UInt64(i) * 250_000_000) }
                    if race.done { race.lost(nil); return }
                    do {
                        let o = try await self.open(host: host, creds: creds)
                        if !race.win(.ok(host, o)) { o.ws.cancel(with: .goingAway, reason: nil) }
                    } catch HandshakeError.unknownDevice {
                        _ = race.win(.unknownDevice)
                    } catch {
                        race.lost("\(host)：\(error.localizedDescription)")
                    }
                }
            }
        }
        guard gen == generation else {
            if case .ok(_, let o) = outcome { o.ws.cancel(with: .goingAway, reason: nil) }
            return
        }
        switch outcome {
        case .ok(let host, let o):
            socket = o.ws
            sealer = o.sealer
            opener = o.opener
            retryDelay = 1
            state = .connected
            onConnectedHost?(host)
            receiveTask = Task { await self.receiveLoop(o.ws, gen) }
            pingTask = Task { await self.pingLoop(gen) }
        case .unknownDevice:
            wantConnected = false
            state = .failed(HandshakeError.unknownDevice.localizedDescription)
            onUnpaired?()
        case .failed(let e):
            state = .failed(e)
            scheduleReconnect()
        }
    }

    private func open(host: String, creds: MacCredentials) async throws -> Opened {
        guard let url = LoopMeEndpoint.url(host: host, port: creds.port, relay: creds.relay, kind: .session) else { throw HandshakeError.badHello }
        let ws = session.webSocketTask(with: url)
        ws.maximumMessageSize = 32 << 20
        ws.resume()
        let nonce = LoopMeCrypto.randomBytes(16)
        let hello = try jsonString(["t": "hello", "deviceId": creds.deviceId, "nonce": nonce.base64EncodedString()])
        try await withSocketTimeout(5, ws) { try await ws.send(.string(hello)) }
        let reply = try await withSocketTimeout(8, ws) { try await receiveJSON(ws) }
        if reply["t"] as? String == "error" {
            ws.cancel(with: .normalClosure, reason: nil)
            if reply["code"] as? String == "unknown_device" { throw HandshakeError.unknownDevice }
            throw HandshakeError.server(reply["message"] as? String ?? "握手失败")
        }
        guard reply["t"] as? String == "hello",
              let sn = (reply["nonce"] as? String).flatMap({ Data(loopmeBase64: $0) }),
              let priv = Data(loopmeBase64: creds.privateKey),
              let macPub = Data(loopmeBase64: creds.macPublicKey) else {
            ws.cancel(with: .normalClosure, reason: nil)
            throw HandshakeError.badHello
        }
        let key = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: priv)
        let keys = try LoopMeCrypto.sessionKeys(privateKey: key, peerPublicKey: macPub, clientNonce: nonce, serverNonce: sn)
        return Opened(ws: ws, sealer: FrameSealer(key: keys.c2s), opener: FrameOpener(key: keys.s2c))
    }

    private func receiveLoop(_ ws: URLSessionWebSocketTask, _ gen: Int) async {
        while gen == generation {
            do {
                let msg = try await ws.receive()
                guard gen == generation else { return }
                guard case .data(let frame) = msg, let opener else { continue }
                let plain = try opener.open(frame)
                handleFrame(plain)
            } catch {
                if gen == generation { connectionLost(error.localizedDescription) }
                return
            }
        }
    }

    private func pingLoop(_ gen: Int) async {
        while gen == generation {
            try? await Task.sleep(nanoseconds: 25_000_000_000)
            guard gen == generation, state == .connected else { return }
            do {
                _ = try await call("ping", timeout: 10)
            } catch {
                if gen == generation { connectionLost("与 Mac 的连接中断") }
                return
            }
        }
    }

    private func handleFrame(_ data: Data) {
        guard let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return }
        if let event = obj["event"] as? String {
            let seq = (obj["seq"] as? NSNumber)?.int64Value ?? 0
            let payload = (try? JSONSerialization.data(withJSONObject: obj["data"] ?? NSNull(), options: [.fragmentsAllowed])) ?? Data("null".utf8)
            onEvent?(event, seq, payload)
        } else if let id = obj["id"] as? String, let cont = pending.removeValue(forKey: id) {
            if let err = obj["error"] as? [String: Any] {
                cont.resume(throwing: RPCError(message: err["message"] as? String ?? "出错了"))
            } else {
                let result = (try? JSONSerialization.data(withJSONObject: obj["result"] ?? NSNull(), options: [.fragmentsAllowed])) ?? Data("null".utf8)
                cont.resume(returning: result)
            }
        }
    }

    private func connectionLost(_ reason: String) {
        teardown(reason)
        state = .failed(reason)
        scheduleReconnect()
    }

    private func scheduleReconnect() {
        guard wantConnected else { return }
        let delay = retryDelay
        retryDelay = min(retryDelay * 2, 30)
        reconnectTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            guard !Task.isCancelled else { return }
            self?.startConnecting()
        }
    }

    private func teardown(_ reason: String) {
        generation += 1
        receiveTask?.cancel()
        pingTask?.cancel()
        reconnectTask?.cancel()
        receiveTask = nil
        pingTask = nil
        reconnectTask = nil
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
        sealer = nil
        opener = nil
        lastSend = nil
        let waiting = pending
        pending = [:]
        for (_, c) in waiting { c.resume(throwing: ConnectionError(message: reason)) }
    }
}

/// First result wins; the rest are dropped. Only touched from main-actor tasks.
private final class Race<T>: @unchecked Sendable {
    private var cont: CheckedContinuation<T, Never>?
    private var remaining: Int
    private var errors: [String] = []
    private let failed: ([String]) -> T

    var done: Bool { cont == nil }

    init(_ cont: CheckedContinuation<T, Never>, count: Int, failed: @escaping ([String]) -> T) {
        self.cont = cont
        remaining = count
        self.failed = failed
        if count == 0 { finish(failed(["没有可用的地址"])) }
    }

    /// Returns false when someone already won.
    func win(_ v: T) -> Bool {
        remaining -= 1
        guard cont != nil else { return false }
        finish(v)
        return true
    }

    func lost(_ error: String?) {
        remaining -= 1
        if let error { errors.append(error) }
        if remaining <= 0, cont != nil { finish(failed(errors)) }
    }

    private func finish(_ v: T) {
        let c = cont
        cont = nil
        c?.resume(returning: v)
    }
}
