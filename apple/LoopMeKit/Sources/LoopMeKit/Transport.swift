import CryptoKit
import Foundation
import Network
#if canImport(Darwin)
import Darwin
#endif

/// How a phone reaches its Mac through the relay (`daemon/src/relay.rs`), from the v2 pairing QR code.
public struct RelayInfo: Codable, Equatable {
    /// The id the relay routes by.
    public var macId: String
    /// Bearer token the relay checks before forwarding anything.
    public var token: String
    /// SHA-256 of the relay's certificate (lowercase hex). `nil` = plain `ws://` (tests only).
    public var fingerprint: String?

    public init(macId: String, token: String, fingerprint: String?) {
        self.macId = macId
        self.token = token
        self.fingerprint = fingerprint
    }
}

public enum LoopMeEndpoint {
    public enum Kind: String { case pair, session }

    /// `wss://host:port/v1/phone/<mac>/<kind>?t=…` through the relay, or the old direct `ws://host:port/v1[/pair]`.
    public static func url(host: String, port: Int, relay: RelayInfo?, kind: Kind) -> URL? {
        let h = host.contains(":") && !host.hasPrefix("[") ? "[\(host)]" : host
        guard let relay else {
            return URL(string: "ws://\(h):\(port)/v1" + (kind == .pair ? "/pair" : ""))
        }
        var c = URLComponents()
        c.scheme = relay.fingerprint == nil ? "ws" : "wss"
        c.host = host
        c.port = port
        c.path = "/v1/phone/\(relay.macId)/\(kind.rawValue)"
        c.queryItems = [URLQueryItem(name: "t", value: relay.token)]
        return c.url
    }

    public static func healthURL(host: String, port: Int, relay: RelayInfo) -> URL? {
        var c = URLComponents()
        c.scheme = relay.fingerprint == nil ? "http" : "https"
        c.host = host
        c.port = port
        c.path = "/healthz"
        c.queryItems = [URLQueryItem(name: "mac", value: relay.macId), URLQueryItem(name: "t", value: relay.token)]
        return c.url
    }

    /// A session that trusts exactly the relay certificate pinned at pairing (no CA, no host name check).
    public static func session(pinning fingerprint: String?, timeout: TimeInterval = 15) -> URLSession {
        let cfg = URLSessionConfiguration.ephemeral
        cfg.waitsForConnectivity = false
        cfg.timeoutIntervalForRequest = timeout
        cfg.requestCachePolicy = .reloadIgnoringLocalCacheData
        guard let fingerprint else { return URLSession(configuration: cfg) }
        return URLSession(configuration: cfg, delegate: PinnedTrust(fingerprint: fingerprint), delegateQueue: nil)
    }
}

/// Accepts a TLS server only when its leaf certificate's SHA-256 matches the pinned value.
final class PinnedTrust: NSObject, URLSessionTaskDelegate {
    let fingerprint: String
    /// Set when a server presented a different certificate (for diagnostics).
    private(set) var mismatch = false

    init(fingerprint: String) {
        self.fingerprint = fingerprint.lowercased()
    }

    func urlSession(_ session: URLSession, didReceive challenge: URLAuthenticationChallenge,
                    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        decide(challenge, completionHandler)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge,
                    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        decide(challenge, completionHandler)
    }

    private func decide(_ challenge: URLAuthenticationChallenge,
                        _ done: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = challenge.protectionSpace.serverTrust else {
            done(.performDefaultHandling, nil)
            return
        }
        guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate], let leaf = chain.first else {
            done(.cancelAuthenticationChallenge, nil)
            return
        }
        let der = SecCertificateCopyData(leaf) as Data
        let hex = SHA256.hash(data: der).map { String(format: "%02x", $0) }.joined()
        if hex == fingerprint {
            done(.useCredential, URLCredential(trust: trust))
        } else {
            mismatch = true
            done(.cancelAuthenticationChallenge, nil)
        }
    }
}

// MARK: - Diagnostics

/// What "设置 → 连接诊断" shows: one row per address, plus a one-line verdict.
public struct ConnectionReport: Equatable {
    public struct Row: Identifiable, Equatable {
        public var id: String { host }
        public var host: String
        /// TCP to host:port: `通` / `超时` / `被拒绝` / an error.
        public var tcp: String
        public var tcpOK: Bool
        /// Relay health: nil when TCP failed.
        public var relay: String?
        public var relayOK: Bool
        public var macOnline: Bool?
    }

    public var rows: [Row]
    public var phoneAddresses: [String]
    public var onVPN: Bool
    public var verdict: String
    public var port: Int
    public var createdAt: Date

    /// Plain text for 复制报告.
    public var text: String {
        var lines = ["LoopMe 连接诊断 \(ISO8601DateFormatter().string(from: createdAt))",
                     "结论：\(verdict)",
                     "手机地址：\(phoneAddresses.joined(separator: ", "))\(onVPN ? "（有 VPN 网卡）" : "（没有 VPN 网卡）")",
                     "端口：\(port)"]
        for r in rows {
            var l = "\(r.host)：TCP \(r.tcp)"
            if let relay = r.relay { l += "；Relay \(relay)" }
            if let m = r.macOnline { l += "；Mac \(m ? "在线" : "不在线")" }
            lines.append(l)
        }
        return lines.joined(separator: "\n")
    }
}

public enum LoopMeDiagnostics {
    public static func run(_ creds: MacCredentials) async -> ConnectionReport {
        let addrs = phoneAddresses()
        let onVPN = addrs.contains { ["utun", "ipsec", "ppp", "tun", "tap"].contains(where: $0.hasPrefix) }
        var rows: [ConnectionReport.Row] = []
        await withTaskGroup(of: ConnectionReport.Row.self) { group in
            for host in creds.hosts {
                group.addTask { await check(host: host, creds: creds) }
            }
            for await r in group { rows.append(r) }
        }
        rows.sort { a, b in (creds.hosts.firstIndex(of: a.host) ?? 0) < (creds.hosts.firstIndex(of: b.host) ?? 0) }
        return ConnectionReport(rows: rows, phoneAddresses: addrs, onVPN: onVPN,
                                verdict: verdict(rows, onVPN: onVPN, relay: creds.relay != nil),
                                port: creds.port, createdAt: Date())
    }

    static func verdict(_ rows: [ConnectionReport.Row], onVPN: Bool, relay: Bool) -> String {
        if rows.contains(where: { $0.macOnline == true }) { return "连接正常" }
        if !relay { return "这是旧版配对。请在 Mac 菜单栏的 LoopMe 里重新显示二维码，再扫一次。" }
        if rows.contains(where: { $0.relayOK && $0.macOnline == false }) {
            return "Relay 正常，但 Mac 上的 LoopMe 没连上它。看一下 Mac 菜单栏里 LoopMe 的状态。"
        }
        if rows.contains(where: { $0.tcpOK && !$0.relayOK && ($0.relay ?? "").contains("证书") }) {
            return "到得了 Mac，但证书和配对时不一致。请重新扫码配对。"
        }
        if rows.contains(where: { $0.tcpOK }) {
            return "到得了 Mac，但 Relay 没有正常响应。确认 Mac 上 LoopMe 正在运行。"
        }
        if rows.contains(where: { $0.tcp == "被拒绝" }) {
            return "到得了 Mac，但这个端口没有服务。确认 Mac 上 LoopMe 正在运行。"
        }
        if !onVPN { return "手机好像没有连公司 VPN。连上后再试。" }
        return "网络到不了这台 Mac：请求没有到达。把这份报告发给 IT，请他们放行 VPN 到这台 Mac 的端口。"
    }

    static func check(host: String, creds: MacCredentials) async -> ConnectionReport.Row {
        let tcp = await tcpProbe(host: host, port: creds.port)
        var row = ConnectionReport.Row(host: host, tcp: tcp.label, tcpOK: tcp.ok, relay: nil, relayOK: false, macOnline: nil)
        guard tcp.ok, let relay = creds.relay, let url = LoopMeEndpoint.healthURL(host: host, port: creds.port, relay: relay) else {
            return row
        }
        let delegate = relay.fingerprint.map { PinnedTrust(fingerprint: $0) }
        let cfg = URLSessionConfiguration.ephemeral
        cfg.timeoutIntervalForRequest = 5
        let session = URLSession(configuration: cfg, delegate: delegate, delegateQueue: nil)
        defer { session.finishTasksAndInvalidate() }
        do {
            let (data, _) = try await session.data(from: url)
            let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
            row.relayOK = obj?["ok"] as? Bool == true
            row.relay = row.relayOK ? "正常" : "响应异常"
            row.macOnline = obj?["macOnline"] as? Bool
        } catch {
            row.relay = delegate?.mismatch == true ? "证书不匹配" : error.localizedDescription
        }
        return row
    }

    /// A bare TCP connect, to tell "the network drops it" (超时) from "nothing listens" (被拒绝).
    static func tcpProbe(host: String, port: Int) async -> (ok: Bool, label: String) {
        guard let p = NWEndpoint.Port(rawValue: UInt16(clamping: port)) else { return (false, "端口无效") }
        let conn = NWConnection(host: NWEndpoint.Host(host), port: p, using: .tcp)
        return await withCheckedContinuation { cont in
            let box = ResumeOnce(cont)
            conn.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    box.resume((true, "通"))
                    conn.cancel()
                case .waiting(let e), .failed(let e):
                    if case .posix(let code) = e, code == .ECONNREFUSED {
                        box.resume((false, "被拒绝"))
                    } else if case .waiting = state {
                        return // keep waiting until the timeout
                    } else {
                        box.resume((false, e.localizedDescription))
                    }
                    conn.cancel()
                default:
                    break
                }
            }
            conn.start(queue: .global())
            DispatchQueue.global().asyncAfter(deadline: .now() + 4) {
                box.resume((false, "超时"))
                conn.cancel()
            }
        }
    }

    /// The phone's own IPv4 addresses as `interface ip`.
    public static func phoneAddresses() -> [String] {
        var out: [String] = []
        var ifap: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&ifap) == 0, let first = ifap else { return out }
        defer { freeifaddrs(ifap) }
        for p in sequence(first: first, next: { $0.pointee.ifa_next }) {
            guard let sa = p.pointee.ifa_addr, sa.pointee.sa_family == UInt8(AF_INET) else { continue }
            let name = String(cString: p.pointee.ifa_name)
            if name.hasPrefix("lo") { continue }
            var buf = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            if getnameinfo(sa, socklen_t(sa.pointee.sa_len), &buf, socklen_t(buf.count), nil, 0, NI_NUMERICHOST) == 0 {
                out.append("\(name) \(String(cString: buf))")
            }
        }
        return out
    }
}

/// Resumes a continuation at most once (NWConnection reports several states).
final class ResumeOnce<T>: @unchecked Sendable {
    private var cont: CheckedContinuation<T, Never>?
    private let lock = NSLock()
    init(_ c: CheckedContinuation<T, Never>) { cont = c }
    func resume(_ v: T) {
        lock.lock()
        let c = cont
        cont = nil
        lock.unlock()
        c?.resume(returning: v)
    }
}
