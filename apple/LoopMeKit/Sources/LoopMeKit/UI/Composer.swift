#if canImport(UIKit)
import SwiftUI
import UIKit

/// The iOS message box, backed by a `UITextView` so sending can commit the input method's
/// in-progress text (pinyin) and clear the box without ending editing. The keyboard stays up
/// after sending, as in Messages, so the chat above doesn't jump.
@MainActor
public final class ComposerController: ObservableObject {
    @Published public private(set) var text = ""
    @Published public private(set) var isFocused = false
    fileprivate weak var textView: UITextView?

    public init() {}

    /// Commits any in-progress input, returns the whole text and empties the box. Focus stays.
    public func take() -> String {
        guard let tv = textView else {
            defer { text = "" }
            return text
        }
        tv.unmarkText()
        let t = tv.text ?? ""
        tv.text = ""
        text = ""
        return t
    }

    public func focus() {
        textView?.becomeFirstResponder()
    }

    fileprivate func changed(_ tv: UITextView) {
        text = tv.text ?? ""
    }

    fileprivate func setFocused(_ f: Bool) {
        if isFocused != f { isFocused = f }
    }
}

public struct ComposerTextView: UIViewRepresentable {
    @ObservedObject var controller: ComposerController
    var maxLines: Int = 6

    public init(controller: ComposerController, maxLines: Int = 6) {
        self.controller = controller
        self.maxLines = maxLines
    }

    public func makeUIView(context: Context) -> UITextView {
        let tv = UITextView()
        tv.delegate = context.coordinator
        tv.font = UIFont.preferredFont(forTextStyle: .body)
        tv.adjustsFontForContentSizeCategory = true
        tv.backgroundColor = .clear
        tv.textColor = UIColor(Theme.text)
        tv.textContainerInset = UIEdgeInsets(top: 10, left: 14, bottom: 10, right: 14)
        tv.textContainer.lineFragmentPadding = 0
        tv.isScrollEnabled = false
        tv.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        controller.textView = tv
        return tv
    }

    public func updateUIView(_ tv: UITextView, context: Context) {
        controller.textView = tv
        // Only follow the controller when it was cleared from outside; never touch text the input method is composing.
        if tv.markedTextRange == nil, tv.text != controller.text {
            tv.text = controller.text
        }
    }

    /// Grows with the text up to `maxLines`, then scrolls inside.
    public func sizeThatFits(_ proposal: ProposedViewSize, uiView tv: UITextView, context: Context) -> CGSize? {
        let width = proposal.width ?? tv.bounds.width
        guard width > 0 else { return nil }
        let fit = tv.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height
        let line = tv.font?.lineHeight ?? 20
        let maxHeight = ceil(line * CGFloat(maxLines)) + tv.textContainerInset.top + tv.textContainerInset.bottom
        let scroll = fit > maxHeight
        if tv.isScrollEnabled != scroll {
            DispatchQueue.main.async { tv.isScrollEnabled = scroll }
        }
        return CGSize(width: width, height: min(fit, maxHeight))
    }

    public func makeCoordinator() -> Coordinator { Coordinator(controller) }

    public final class Coordinator: NSObject, UITextViewDelegate {
        let controller: ComposerController
        init(_ c: ComposerController) { controller = c }

        public func textViewDidChange(_ tv: UITextView) {
            MainActor.assumeIsolated { controller.changed(tv) }
        }

        public func textViewDidBeginEditing(_ tv: UITextView) {
            MainActor.assumeIsolated { controller.setFocused(true) }
        }

        public func textViewDidEndEditing(_ tv: UITextView) {
            MainActor.assumeIsolated { controller.setFocused(false) }
        }
    }
}
#endif
