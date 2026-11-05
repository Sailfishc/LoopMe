import CryptoKit
import Foundation

public struct RPCError: LocalizedError {
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
    private let session: URLSession

    public init() {
        let cfg = URLSessionConfiguration.default
        cfg.waitsForConnectivity = false
        cfg.timeoutIntervalForRequest = 15
        session = URLSession(configuration: cfg)
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
    public func reconnectNow() {
        guard wantConnected, state != .connected, state != .connecting else { return }
        retryDelay = 1
        startConnecting()
    }

    // MARK: RPC

    public func call(_ method: String, _ params: [String: Any] = [:]) async throws -> Data {
        guard state == .connected, let sealer else { throw RPCError(message: "未连接到 Mac") }
        let id = UUID().uuidString
        let body = try JSONSerialization.data(withJSONObject: ["id": id, "method": method, "params": params])
        let frame = try sealer.seal(body)
        return try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Data, Error>) in
            pending[id] = cont
            enqueueSend(frame) { [weak self] error in
                self?.pending.removeValue(forKey: id)?.resume(throwing: error)
            }
            Task { [weak self] in
                try? await Task.sleep(nanoseconds: 30_000_000_000)
                self?.pending.removeValue(forKey: id)?.resume(throwing: RPCError(message: "Mac 没有响应"))
            }
        }
    }

    /// Calls and decodes the result.
    public func call<T: Decodable>(_ method: String, _ params: [String: Any] = [:], as type: T.Type) async throws -> T {
        let data = try await call(method, params)
        return try JSONDecoder().decode(T.self, from: data)
    }

    // MARK: internals

    private func enqueueSend(_ frame: Data, onError: @escaping (Error) -> Void) {
        let prev = lastSend
        let ws = socket
        lastSend = Task {
            await prev?.value
            do {
                guard let ws else { throw RPCError(message: "未连接到 Mac") }
                try await ws.send(.data(frame))
            } catch {
                onError(error)
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

    private func attempt(_ creds: MacCredentials, _ gen: Int) async {
        var lastError = "无法连接到 Mac"
        for host in creds.hosts {
            guard gen == generation else { return }
            do {
                try await handshake(host: host, creds: creds, gen: gen)
                return
            } catch HandshakeError.unknownDevice {
                wantConnected = false
                state = .failed(HandshakeError.unknownDevice.localizedDescription)
                onUnpaired?()
                return
            } catch {
                lastError = "\(host)：\(error.localizedDescription)"
            }
        }
        guard gen == generation else { return }
        state = .failed(lastError)
        scheduleReconnect()
    }

    private func handshake(host: String, creds: MacCredentials, gen: Int) async throws {
        guard let url = URL(string: "ws://\(host):\(creds.port)/v1") else { throw HandshakeError.badHello }
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
        guard gen == generation else {
            ws.cancel(with: .goingAway, reason: nil)
            return
        }
        socket = ws
        sealer = FrameSealer(key: keys.c2s)
        opener = FrameOpener(key: keys.s2c)
        retryDelay = 1
        state = .connected
        onConnectedHost?(host)
        receiveTask = Task { await self.receiveLoop(ws, gen) }
        pingTask = Task { await self.pingLoop(gen) }
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
                _ = try await call("ping")
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
        for (_, c) in waiting { c.resume(throwing: RPCError(message: reason)) }
    }
}
