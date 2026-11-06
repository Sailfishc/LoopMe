import XCTest
@testable import LoopMeKit

final class PairingLifecycleTests: XCTestCase {
    private func credentials() -> MacCredentials {
        MacCredentials(deviceId: "revoked-phone", privateKey: Data(repeating: 1, count: 32).base64EncodedString(),
                       macPublicKey: Data(repeating: 2, count: 32).base64EncodedString(), macName: "Mac",
                       hosts: ["127.0.0.1"], port: 9)
    }

    @MainActor
    func testRevocationForgetsPairingAndStopsReconnect() async throws {
        var saved: [MacCredentials?] = []
        let store = LoopMeStore(saveCredentials: { saved.append($0) })
        defer { store.stopConnection() }
        store.start(with: credentials())
        store.lastError = "Mac unavailable"
        store.apply(event: "unpaired", seq: 0, data: Data("null".utf8))

        XCTAssertNil(store.credentials)
        XCTAssertEqual(store.connection, .idle)
        XCTAssertNil(store.lastError)
        XCTAssertEqual(saved.count, 1)
        if let last = saved.last { XCTAssertNil(last) }
        try await Task.sleep(nanoseconds: 1_200_000_000)
        XCTAssertEqual(store.connection, .idle, "An in-flight handshake or retry must not reconnect after revocation")
    }

    @MainActor
    func testLocalUnpairDoesNotRequireSyncOrReachableMac() async throws {
        var saved: [MacCredentials?] = []
        let store = LoopMeStore(saveCredentials: { saved.append($0) })
        defer { store.stopConnection() }
        store.start(with: credentials())
        store.lastError = "Mac unavailable"
        XCTAssertFalse(store.hasSynced)
        await store.unpair()

        XCTAssertNil(store.credentials)
        XCTAssertEqual(store.connection, .idle)
        XCTAssertNil(store.lastError)
        XCTAssertEqual(saved.count, 1)
        if let last = saved.last { XCTAssertNil(last) }
    }
}
