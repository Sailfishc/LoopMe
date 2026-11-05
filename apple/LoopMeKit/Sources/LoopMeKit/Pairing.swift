import CryptoKit
import Foundation

/// The content of the QR code shown on the Mac:
/// `loopme://pair?v=1&k=<mac public key>&c=<code>&h=<host,host>&p=<port>&n=<mac name>`.
public struct PairingLink: Equatable {
    public var macPublicKey: Data
    public var code: String
    public var hosts: [String]
    public var port: Int
    public var macName: String

    public init?(string: String) {
        guard let comps = URLComponents(string: string.trimmingCharacters(in: .whitespacesAndNewlines)),
              comps.scheme == "loopme", comps.host == "pair" else { return nil }
        var q: [String: String] = [:]
        for item in comps.queryItems ?? [] { q[item.name] = item.value ?? "" }
        guard let k = q["k"], let key = Data(loopmeBase64: k), key.count == 32,
              let code = q["c"], !code.isEmpty,
              let h = q["h"], let p = q["p"], let port = Int(p) else { return nil }
        macPublicKey = key
        self.code = code
        hosts = h.split(separator: ",").map(String.init).filter { !$0.isEmpty }
        self.port = port
        if let n = q["n"], let d = Data(loopmeBase64: n), let s = String(data: d, encoding: .utf8) {
            macName = s
        } else {
            macName = "Mac"
        }
    }
}

/// Everything needed to reach a paired Mac. Kept in the Keychain on iOS.
public struct MacCredentials: Codable, Equatable {
    public var deviceId: String
    /// Our X25519 private key, base64.
    public var privateKey: String
    /// The Mac's X25519 public key, base64.
    public var macPublicKey: String
    public var macName: String
    public var hosts: [String]
    public var port: Int

    public init(deviceId: String, privateKey: String, macPublicKey: String, macName: String, hosts: [String], port: Int) {
        self.deviceId = deviceId
        self.privateKey = privateKey
        self.macPublicKey = macPublicKey
        self.macName = macName
        self.hosts = hosts
        self.port = port
    }

    /// Reads the file the daemon writes for the Mac app (`local-client.json`).
    public static func local(dataDirectory: URL) -> MacCredentials? {
        struct LocalFile: Decodable { let deviceId: String; let privateKey: String; let macPublicKey: String; let port: Int }
        guard let data = try? Data(contentsOf: dataDirectory.appendingPathComponent("local-client.json")),
              let f = try? JSONDecoder().decode(LocalFile.self, from: data) else { return nil }
        return MacCredentials(deviceId: f.deviceId, privateKey: f.privateKey, macPublicKey: f.macPublicKey,
                              macName: localMacName(), hosts: ["127.0.0.1"], port: f.port)
    }
}

func localMacName() -> String {
    #if os(macOS)
    return Host.current().localizedName ?? "Mac"
    #else
    return "Mac"
    #endif
}

public enum PairingError: LocalizedError {
    case unreachable(String)
    case rejected(String)
    case sasMismatch
    case protocolError

    public var errorDescription: String? {
        switch self {
        case .unreachable(let s): return "连不上 Mac：\(s)。请确认手机已连上公司 VPN 或与 Mac 在同一网络。"
        case .rejected(let s): return s
        case .sasMismatch: return "校验码不一致，已中止配对。"
        case .protocolError: return "Mac 返回了无法识别的数据。"
        }
    }
}

public enum LoopMePairing {
    /// Pairs with the Mac in `link`. `onSAS` is called with the 4-digit code to show while the user confirms on the Mac.
    public static func pair(link: PairingLink, deviceName: String, onSAS: @escaping (String) -> Void) async throws -> MacCredentials {
        let key = Curve25519.KeyAgreement.PrivateKey()
        let pub = key.publicKey.rawRepresentation
        var lastError = "没有可用的地址"
        for host in link.hosts {
            guard let url = URL(string: "ws://\(host):\(link.port)/v1/pair") else { continue }
            let task = URLSession.shared.webSocketTask(with: url)
            task.resume()
            do {
                let hello: [String: Any] = ["t": "pair", "code": link.code, "devicePub": pub.base64EncodedString(), "deviceName": deviceName]
                try await withSocketTimeout(5, task) { try await task.send(.string(jsonString(hello))) }
                let first = try await withSocketTimeout(8, task) { try await receiveJSON(task) }
                guard first["t"] as? String == "pair_pending", let sas = first["sas"] as? String else {
                    task.cancel(with: .normalClosure, reason: nil)
                    throw PairingError.rejected(first["message"] as? String ?? "配对被拒绝")
                }
                guard sas == LoopMeCrypto.sas(macPublicKey: link.macPublicKey, devicePublicKey: pub) else {
                    task.cancel(with: .normalClosure, reason: nil)
                    throw PairingError.sasMismatch
                }
                onSAS(sas)
                let done = try await withSocketTimeout(130, task) { try await receiveJSON(task) }
                task.cancel(with: .normalClosure, reason: nil)
                guard done["t"] as? String == "paired", let deviceId = done["deviceId"] as? String else {
                    throw PairingError.rejected(done["message"] as? String ?? "Mac 上没有确认配对")
                }
                // Put the host that worked first.
                var hosts = link.hosts
                hosts.removeAll { $0 == host }
                hosts.insert(host, at: 0)
                return MacCredentials(deviceId: deviceId,
                                      privateKey: key.rawRepresentation.base64EncodedString(),
                                      macPublicKey: link.macPublicKey.base64EncodedString(),
                                      macName: done["macName"] as? String ?? link.macName,
                                      hosts: hosts,
                                      port: link.port)
            } catch let e as PairingError {
                throw e
            } catch {
                task.cancel(with: .goingAway, reason: nil)
                lastError = "\(host)：\(error.localizedDescription)"
            }
        }
        throw PairingError.unreachable(lastError)
    }
}

// MARK: - small helpers shared with the connection

struct TimeoutError: LocalizedError {
    var errorDescription: String? { "连接超时" }
}

final class TimeoutFlag: @unchecked Sendable {
    var fired = false
}

/// Runs `op`; if it takes longer than `seconds`, cancels `socket` (which fails the pending call) and throws `TimeoutError`.
func withSocketTimeout<T>(_ seconds: Double, _ socket: URLSessionWebSocketTask, _ op: () async throws -> T) async throws -> T {
    let flag = TimeoutFlag()
    let timer = Task { [weak socket] in
        try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
        if Task.isCancelled { return }
        flag.fired = true
        socket?.cancel(with: .goingAway, reason: nil)
    }
    defer { timer.cancel() }
    do {
        return try await op()
    } catch {
        if flag.fired { throw TimeoutError() }
        throw error
    }
}

func jsonString(_ obj: [String: Any]) throws -> String {
    let d = try JSONSerialization.data(withJSONObject: obj)
    return String(data: d, encoding: .utf8) ?? "{}"
}

func receiveJSON(_ task: URLSessionWebSocketTask) async throws -> [String: Any] {
    let msg = try await task.receive()
    let data: Data
    switch msg {
    case .string(let s): data = Data(s.utf8)
    case .data(let d): data = d
    @unknown default: throw PairingError.protocolError
    }
    guard let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any] else { throw PairingError.protocolError }
    return obj
}
