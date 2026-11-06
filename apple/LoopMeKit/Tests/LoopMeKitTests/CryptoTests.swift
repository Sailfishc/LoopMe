import CryptoKit
import XCTest
@testable import LoopMeKit

/// The same vector is asserted in `daemon/src/crypto.rs` (`cross_platform_vector`).
final class CryptoTests: XCTestCase {
    func testCrossPlatformVector() throws {
        let mac = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: Data((1...32).map { UInt8($0) }))
        let dev = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: Data((101...132).map { UInt8($0) }))
        XCTAssertEqual(mac.publicKey.rawRepresentation.base64EncodedString(), "B6N8vBQgk8i3VdwbEOhstCY3StFqqFPtC9/AsrhtHHw=")
        XCTAssertEqual(dev.publicKey.rawRepresentation.base64EncodedString(), "VxR2nRFr92Q2rnS8eT0sMK0ZA8WaxSc4BcfiaYtBDDY=")

        let keys = try LoopMeCrypto.sessionKeys(privateKey: dev,
                                                peerPublicKey: mac.publicKey.rawRepresentation,
                                                clientNonce: Data(repeating: 7, count: 16),
                                                serverNonce: Data(repeating: 9, count: 16))
        XCTAssertEqual(keys.c2s.withUnsafeBytes { Data($0) }.base64EncodedString(), "qi6TOdRLZWDhxkfbYMzVK32BvUi4lsSL0x8x40Xe8HY=")
        XCTAssertEqual(keys.s2c.withUnsafeBytes { Data($0) }.base64EncodedString(), "qxUFz+Y9wMnGIEunFVSKdDPBV0cgQM/XCpNUFCjr5ew=")

        let frame = try FrameSealer(key: keys.c2s).seal(Data("hello".utf8))
        XCTAssertEqual(frame.base64EncodedString(), "AAAAAAAAAAAAAAAA83T5AE3+p7Bg+CcHg/9Egoq2yH6q")

        let opener = FrameOpener(key: keys.c2s)
        XCTAssertEqual(try opener.open(frame), Data("hello".utf8))
        XCTAssertThrowsError(try opener.open(frame), "replay must fail")

        XCTAssertEqual(LoopMeCrypto.sas(macPublicKey: mac.publicKey.rawRepresentation,
                                        devicePublicKey: dev.publicKey.rawRepresentation), "5832")
    }

    func testPairingLinkParsing() throws {
        let url = "loopme://pair?v=1&k=B6N8vBQgk8i3VdwbEOhstCY3StFqqFPtC9_AsrhtHHw&c=abc123&h=10.0.0.5,mac.local&p=7788&n=TWFj"
        let link = try XCTUnwrap(PairingLink(string: url))
        XCTAssertEqual(link.hosts, ["10.0.0.5", "mac.local"])
        XCTAssertEqual(link.port, 7788)
        XCTAssertEqual(link.code, "abc123")
        XCTAssertEqual(link.macName, "Mac")
        XCTAssertEqual(link.macPublicKey.base64EncodedString(), "B6N8vBQgk8i3VdwbEOhstCY3StFqqFPtC9/AsrhtHHw=")
    }
}
