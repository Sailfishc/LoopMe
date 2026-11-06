import XCTest
@testable import LoopMeKit

/// JSON shapes as the daemon sends them (`daemon/src/model.rs`).
final class ModelTests: XCTestCase {
    func testOfferMessageDecodes() throws {
        let json = """
        {"id":"msg_1","seq":7,"role":"agent","text":"你的会话停在 2 个测试失败。","itemId":null,"decisionId":null,
         "createdAt":1793936248724,"streaming":false,"replyTo":null,"feedback":"down",
         "offer":{"kind":"takeover","label":"可以接手","title":"修好 fix-login 的测试","detail":"停在 2 个失败",
                  "why":"pytest: 2 failed","sessionId":"s1","cwd":"/r","task":"修复并跑测试","branch":null,"base":null,
                  "worktree":null,"actions":[{"id":"take","label":"让我来","style":"primary"}],
                  "state":"open","chosen":null,"note":null}}
        """
        let m = try JSONDecoder().decode(Message.self, from: Data(json.utf8))
        XCTAssertEqual(m.offer?.actions.first?.label, "让我来")
        XCTAssertEqual(m.feedback, "down")
    }

    func testStatusAndSessionDecode() throws {
        let status = """
        {"macName":"Mac","daemonVersion":"0.1.0","codex":{"found":true,"path":"/c","version":"0.166.0","loggedIn":true,"error":null},
         "agent":"working","currentRun":{"id":"run_1","title":"修测试","startedAt":1,"step":"运行 pytest -q","messageId":"msg_1"},
         "atDesk":false,"hooks":"on","pushConfigured":false,"listenPort":7788,"hosts":["127.0.0.1"]}
        """
        let s = try JSONDecoder().decode(Status.self, from: Data(status.utf8))
        XCTAssertEqual(s.currentRun?.step, "运行 pytest -q")
        XCTAssertEqual(s.hooks, "on")

        let session = """
        {"id":"s1","cwd":"/r","title":"修一下登录测试","branch":"fix-login","state":"working","lastPrompt":"修一下登录测试",
         "lastMessage":null,"transcriptPath":null,"startedAt":1,"updatedAt":2,"evalAt":null,"offeredAt":null,"muted":false}
        """
        let c = try JSONDecoder().decode(CodexSession.self, from: Data(session.utf8))
        XCTAssertEqual(Fmt.sessionState(c.state), "正在跑")
    }
}

final class RelayLinkTests: XCTestCase {
    func testV2PairingLinkCarriesRelay() throws {
        let k = Data(repeating: 7, count: 32).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        let link = try XCTUnwrap(PairingLink(string: "loopme://pair?v=2&k=\(k)&c=abc&h=10.0.0.5,mac.local&p=8443&n=TWFj&r=mid1&t=tok123&f=ABCDEF"))
        XCTAssertEqual(link.port, 8443)
        XCTAssertEqual(link.relay, RelayInfo(macId: "mid1", token: "tok123", fingerprint: "abcdef"))
        let url = LoopMeEndpoint.url(host: "10.0.0.5", port: 8443, relay: link.relay, kind: .session)
        XCTAssertEqual(url?.absoluteString, "wss://10.0.0.5:8443/v1/phone/mid1/session?t=tok123")
        // v1 links stay direct.
        let v1 = try XCTUnwrap(PairingLink(string: "loopme://pair?v=1&k=\(k)&c=abc&h=10.0.0.5&p=7788&n=TWFj"))
        XCTAssertNil(v1.relay)
        XCTAssertEqual(LoopMeEndpoint.url(host: "10.0.0.5", port: 7788, relay: nil, kind: .pair)?.absoluteString, "ws://10.0.0.5:7788/v1/pair")
    }

    func testOldCredentialsDecodeWithoutRelay() throws {
        let json = #"{"deviceId":"d","privateKey":"p","macPublicKey":"m","macName":"Mac","hosts":["a"],"port":7788}"#
        let c = try JSONDecoder().decode(MacCredentials.self, from: Data(json.utf8))
        XCTAssertNil(c.relay)
    }

    func testVerdicts() {
        let row = { (tcp: String, ok: Bool) in ConnectionReport.Row(host: "h", tcp: tcp, tcpOK: ok, relay: nil, relayOK: false, macOnline: nil) }
        XCTAssertTrue(LoopMeDiagnostics.verdict([row("超时", false)], onVPN: false, relay: true).contains("VPN"))
        XCTAssertTrue(LoopMeDiagnostics.verdict([row("超时", false)], onVPN: true, relay: true).contains("IT"))
        XCTAssertTrue(LoopMeDiagnostics.verdict([row("被拒绝", false)], onVPN: true, relay: true).contains("端口"))
    }
}
