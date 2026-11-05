import CryptoKit
import Foundation

/// End-to-end encryption, mirroring `daemon/src/crypto.rs`.
///
/// X25519 static-static agreement, HKDF-SHA256 (salt = clientNonce ‖ serverNonce,
/// info `loopme v1 c2s` / `loopme v1 s2c`), ChaCha20-Poly1305 frames in
/// `SealedBox.combined` form with a counter nonce (4 zero bytes ‖ UInt64 LE).
public enum LoopMeCrypto {
    public static let infoC2S = Data("loopme v1 c2s".utf8)
    public static let infoS2C = Data("loopme v1 s2c".utf8)

    public struct SessionKeys {
        public let c2s: SymmetricKey
        public let s2c: SymmetricKey
    }

    public static func sessionKeys(privateKey: Curve25519.KeyAgreement.PrivateKey,
                                   peerPublicKey: Data,
                                   clientNonce: Data,
                                   serverNonce: Data) throws -> SessionKeys {
        let peer = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: peerPublicKey)
        let shared = try privateKey.sharedSecretFromKeyAgreement(with: peer)
        let salt = clientNonce + serverNonce
        let c2s = shared.hkdfDerivedSymmetricKey(using: SHA256.self, salt: salt, sharedInfo: infoC2S, outputByteCount: 32)
        let s2c = shared.hkdfDerivedSymmetricKey(using: SHA256.self, salt: salt, sharedInfo: infoS2C, outputByteCount: 32)
        return SessionKeys(c2s: c2s, s2c: s2c)
    }

    /// Four-digit short authentication string shown on both screens while pairing.
    public static func sas(macPublicKey: Data, devicePublicKey: Data) -> String {
        var h = SHA256()
        h.update(data: Data("loopme v1 sas".utf8))
        h.update(data: macPublicKey)
        h.update(data: devicePublicKey)
        let d = Array(h.finalize())
        let n = (UInt32(d[0]) << 24 | UInt32(d[1]) << 16 | UInt32(d[2]) << 8 | UInt32(d[3])) % 10_000
        return String(format: "%04u", n)
    }

    public static func randomBytes(_ count: Int) -> Data {
        var g = SystemRandomNumberGenerator()
        return Data((0..<count).map { _ in UInt8.random(in: 0...255, using: &g) })
    }

    static func nonce(_ counter: UInt64) -> Data {
        var n = Data(count: 4)
        withUnsafeBytes(of: counter.littleEndian) { n.append(contentsOf: $0) }
        return n
    }
}

public enum LoopMeCryptoError: Error {
    case frameTooShort, badNonce, replay
}

public final class FrameSealer {
    private let key: SymmetricKey
    private var counter: UInt64 = 0

    public init(key: SymmetricKey) { self.key = key }

    public func seal(_ plaintext: Data) throws -> Data {
        let nonce = try ChaChaPoly.Nonce(data: LoopMeCrypto.nonce(counter))
        counter += 1
        return try ChaChaPoly.seal(plaintext, using: key, nonce: nonce).combined
    }
}

public final class FrameOpener {
    private let key: SymmetricKey
    private var nextCounter: UInt64 = 0

    public init(key: SymmetricKey) { self.key = key }

    public func open(_ frame: Data) throws -> Data {
        guard frame.count >= 28 else { throw LoopMeCryptoError.frameTooShort }
        let bytes = [UInt8](frame.prefix(12))
        guard bytes[0] == 0, bytes[1] == 0, bytes[2] == 0, bytes[3] == 0 else { throw LoopMeCryptoError.badNonce }
        var counter: UInt64 = 0
        for i in 0..<8 { counter |= UInt64(bytes[4 + i]) << (8 * UInt64(i)) }
        guard counter >= nextCounter else { throw LoopMeCryptoError.replay }
        let box = try ChaChaPoly.SealedBox(combined: frame)
        let plain = try ChaChaPoly.open(box, using: key)
        nextCounter = counter + 1
        return plain
    }
}

public extension Data {
    /// Decodes standard or URL-safe base64, with or without padding.
    init?(loopmeBase64 s: String) {
        var t = s.trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        while t.count % 4 != 0 { t += "=" }
        self.init(base64Encoded: t)
    }
}
