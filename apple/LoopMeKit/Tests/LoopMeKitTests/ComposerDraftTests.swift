import XCTest
@testable import LoopMeKit

final class ComposerDraftTests: XCTestCase {
    func testSentTextCannotReturnThroughThePreviousKeyboardBinding() {
        var draft = ComposerDraft()
        let oldInput = draft.inputID
        draft.edit("已经选定的中文", from: oldInput)
        draft.sent()
        draft.edit("已经选定的中文", from: oldInput)
        XCTAssertEqual(draft.text, "")

        draft.edit("下一条消息", from: draft.inputID)
        XCTAssertEqual(draft.text, "下一条消息")
    }

    func testCurrentKeyboardCanContinueEditingBeforeSend() {
        var draft = ComposerDraft()
        let input = draft.inputID
        draft.edit("你好", from: input)
        draft.edit("你好，测试", from: input)
        XCTAssertEqual(draft.text, "你好，测试")
    }
}
