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
