import SwiftUI
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

/// The single conversation stream with item and decision cards.
public struct ChatView: View {
    @EnvironmentObject var store: LoopMeStore
    @State private var draft = ComposerDraft()
    @State private var nowExpanded = false
    @State private var selecting: Message?
    @State private var diagnosing = false
    /// Whether the newest message is on screen.
    @State private var atBottom = true
    /// Messages that arrived while scrolled up.
    @State private var unseen = 0
    @State private var scrollRequest = 0
    /// Long agent replies from before this time start folded; newer ones start open. Fixed once, so a new
    /// reply never folds the one above it (that shift made the list jump).
    @State private var openFrom: Int64?
    @State private var showBanner = false
    @FocusState private var inputFocused: Bool

    public init() {}

    public var body: some View {
        VStack(spacing: 0) {
            connectionBanner
            NowBar(expanded: $nowExpanded)
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 6) {
                        if store.messages.isEmpty && store.outgoing.isEmpty {
                            emptyState
                        }
                        let rows = chatRows
                        ForEach(Array(rows.enumerated()), id: \.element.id) { i, row in
                            rowView(row)
                                // A new speaker starts a little further down; consecutive blocks sit close.
                                .padding(.top, i > 0 && rows[i - 1].role != row.role ? 10 : 0)
                                .id(row.id)
                        }
                        if store.isWorking && store.status.currentRun?.messageId == nil && !nowExpanded {
                            WorkingRow().id("working")
                        }
                        // Visible ⇔ the user is reading the newest messages; only then do new ones pull the list down.
                        Color.clear.frame(height: 4).id("bottom")
                            .onAppear { atBottom = true; unseen = 0 }
                            .onDisappear { atBottom = false }
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                }
                .background(Theme.background)
                .defaultScrollAnchor(.bottom)
                #if os(iOS)
                .scrollDismissesKeyboard(.interactively)
                #endif
                .overlay(alignment: .bottomTrailing) {
                    if !atBottom {
                        Button { scrollToBottom(proxy, animated: true) } label: {
                            HStack(spacing: 4) {
                                Image(systemName: "arrow.down")
                                if unseen > 0 { Text("\(unseen) 条新消息") }
                            }
                            .font(.caption.weight(.semibold))
                            .padding(.horizontal, 12).padding(.vertical, 8)
                            .foregroundStyle(Theme.text)
                            .background(Theme.card, in: Capsule())
                            .overlay(Capsule().stroke(Theme.border, lineWidth: 1))
                        }
                        .buttonStyle(.plain)
                        .padding(16)
                        .accessibilityLabel(unseen > 0 ? "\(unseen) 条新消息，回到底部" : "回到底部")
                        .transition(.opacity)
                    }
                }
                .onAppear {
                    markOpenFrom()
                    scrollToBottom(proxy, animated: false)
                }
                .onChange(of: store.hasSynced) { _, _ in markOpenFrom() }
                // A new row: follow it only when already at the bottom (or it's our own message); otherwise count it.
                .onChange(of: chatRows.last?.id) { _, _ in
                    if chatRows.last?.role == "user" || atBottom {
                        scrollToBottom(proxy, animated: true)
                    } else {
                        unseen += 1
                    }
                }
                // Streaming text: stay pinned without animating every token (animated scrolls fight each other and bounce).
                .onChange(of: store.messages.last?.text) { _, _ in
                    if atBottom { scrollToBottom(proxy, animated: false) }
                }
                .onChange(of: scrollRequest) { _, _ in scrollToBottom(proxy, animated: true) }
                .onChange(of: inputFocused) { _, focused in
                    // The keyboard shrinks the list; keep the newest message above it.
                    guard focused, atBottom else { return }
                    Task {
                        try? await Task.sleep(nanoseconds: 300_000_000)
                        scrollToBottom(proxy, animated: true)
                    }
                }
            }
            inputBar
        }
        .background(Theme.background)
        .overlay(alignment: .top) {
            if nowExpanded {
                ZStack(alignment: .top) {
                    Color.black.opacity(0.35)
                        .ignoresSafeArea()
                        .onTapGesture { withAnimation(.easeOut(duration: 0.2)) { nowExpanded = false } }
                    ScrollView {
                        NowPanel(onClose: { withAnimation(.easeOut(duration: 0.2)) { nowExpanded = false } })
                            .padding(.horizontal, 16)
                    }
                    .frame(maxHeight: 460)
                    .fixedSize(horizontal: false, vertical: true)
                }
                .padding(.top, nowBarHeight)
                .transition(.opacity)
            }
        }
        .task(id: store.isConnected) {
            if store.isConnected {
                showBanner = false
            } else {
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                if !Task.isCancelled && !store.isConnected { showBanner = true }
            }
        }
        .onChange(of: store.nowSummary == nil) { _, empty in
            if empty { nowExpanded = false }
        }
        .sheet(item: $selecting) { m in
            SelectTextSheet(text: m.text)
        }
        #if os(iOS)
        .sheet(isPresented: $diagnosing) {
            NavigationStack {
                ConnectionDiagnosticsView()
                    .toolbar { ToolbarItem(placement: .confirmationAction) { Button("完成") { diagnosing = false } } }
            }
        }
        #endif
    }

    /// Room for the connection banner and the "现在" bar above the expanded panel.
    private var nowBarHeight: CGFloat { store.isConnected || !showBanner ? 56 : 92 }

    /// One row per message; a message still on its way keeps the same identity once the Mac has it.
    private var chatRows: [ChatRow] {
        let shown = store.messages.filter(isShown).map { ChatRow.message($0, key: store.rowKeys[$0.id] ?? $0.id) }
        return shown + store.outgoing.map(ChatRow.outgoing)
    }

    @ViewBuilder private func rowView(_ row: ChatRow) -> some View {
        switch row {
        case .message(let m, _):
            MessageRow(message: m, latest: m.createdAt >= (openFrom ?? 0),
                       onSelectText: { selecting = $0 }, onReply: { reply(to: $0) })
        case .outgoing(let o):
            HStack {
                Spacer(minLength: 48)
                VStack(alignment: .trailing, spacing: 4) {
                    UserBubble(text: o.text, quote: store.message(o.replyTo).map { $0.offer?.title ?? $0.text })
                        .opacity(o.failed ? 0.55 : 1)
                    if o.failed {
                        Button { store.resend(o.id) } label: {
                            Label("没有发出去，点按重试", systemImage: "exclamationmark.circle")
                                .font(.caption2.weight(.semibold))
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(.red)
                        .disabled(!store.isConnected)
                        .contextMenu {
                            Button(role: .destructive) { store.discardOutgoing(o.id) } label: { Label("删除", systemImage: "trash") }
                        }
                    }
                }
            }
        }
    }

    private func markOpenFrom() {
        guard openFrom == nil, store.hasSynced else { return }
        openFrom = store.messages.last(where: { $0.role == "agent" })?.createdAt ?? 0
    }

    private func scrollToBottom(_ proxy: ScrollViewProxy, animated: Bool) {
        unseen = 0
        if animated {
            withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
        } else {
            var t = Transaction()
            t.disablesAnimations = true
            withTransaction(t) { proxy.scrollTo("bottom", anchor: .bottom) }
        }
    }

    /// Suggestions come one at a time; the others wait behind 换一个.
    private func isShown(_ m: Message) -> Bool {
        guard m.role == "system", let iid = m.itemId, let item = store.items[iid], item.status == "suggested" else { return true }
        return store.currentSuggestion?.id == item.id
    }

    private func reply(to m: Message) {
        store.replyTo = m
        inputFocused = true
    }

    /// Only after a short grace period: coming back from the lock screen reconnects in about a second,
    /// and a banner that flashes in and out would push the whole list down and up.
    @ViewBuilder private var connectionBanner: some View {
        if !store.isConnected && showBanner {
            let trying = store.connection == .connecting || store.connection == .idle
            HStack(spacing: 6) {
                if trying { ProgressView().controlSize(.mini) }
                Text(trying ? "正在连接 Mac…" : "Mac 不可达，正在重试。草稿会保留。")
                    .font(.caption)
                    .lineLimit(1)
                Spacer()
                #if os(iOS)
                if case .failed = store.connection {
                    Button("诊断") { diagnosing = true }.font(.caption.weight(.semibold))
                }
                #endif
                Button("重试") { store.conn.reconnectNow() }.font(.caption.weight(.semibold))
            }
            .padding(.horizontal, 16).padding(.vertical, 8)
            .foregroundStyle(Theme.secondary)
            .background(Theme.card)
        }
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("你好，我在你的 Mac 上。").font(.title3.weight(.semibold)).foregroundStyle(Theme.text)
            Text("告诉我要盯住什么，例如“帮我盯着 PR #12 的 CI，变绿告诉我”。我会定时检查，有结果或需要你决定时再来找你。")
                .font(.subheadline).foregroundStyle(Theme.secondary)
            Button("分析我最近的 Codex 会话") { store.analyzeSessions() }
                .buttonStyle(PillButtonStyle(prominent: false))
                .disabled(!store.isConnected)
        }
        .padding(.top, 24)
    }

    private var inputBar: some View {
        VStack(spacing: 6) {
            if let q = store.replyTo {
                HStack(spacing: 8) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("回复 · 这条会作为上下文一起发给 Agent").font(.caption.weight(.semibold)).foregroundStyle(Theme.accent)
                        Text(q.offer?.title ?? q.text).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                    }
                    Spacer()
                    Button { store.replyTo = nil } label: { Image(systemName: "xmark.circle.fill") }
                        .buttonStyle(.plain)
                        .foregroundStyle(Theme.secondary)
                        .accessibilityLabel("取消回复")
                }
                .padding(.horizontal, 12).padding(.vertical, 8)
                .background(Theme.card, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                .overlay(alignment: .leading) {
                    RoundedRectangle(cornerRadius: 2).fill(Theme.accent).frame(width: 3).padding(.vertical, 6)
                }
            }
            inputRow
        }
        .padding(.horizontal, 12).padding(.vertical, 10)
        .background(Theme.background)
    }

    private var inputRow: some View {
        let inputID = draft.inputID
        let text = Binding(get: { draft.text }, set: { draft.edit($0, from: inputID) })
        return HStack(alignment: .bottom, spacing: 10) {
            TextField("和 Agent 说点什么", text: text, axis: .vertical)
                .id(inputID)
                .textFieldStyle(.plain)
                .lineLimit(1...6)
                .focused($inputFocused)
                .padding(.horizontal, 14).padding(.vertical, 10)
                .foregroundStyle(Theme.text)
                .background(Theme.card, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
                .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).stroke(Theme.border, lineWidth: 1))
                .onSubmit(send)
            if store.isWorking && draft.text.isEmpty {
                Button(action: { store.stopRun() }) {
                    Image(systemName: "stop.circle.fill").font(.system(size: 30))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Theme.text)
                .help("停止当前工作")
            } else {
                Button(action: send) {
                    Image(systemName: "arrow.up.circle.fill").font(.system(size: 30))
                }
                .buttonStyle(.plain)
                .foregroundStyle(canSend ? Theme.text : Theme.secondary.opacity(0.5))
                .disabled(!canSend)
                .keyboardShortcut(.return, modifiers: .command)
            }
        }
    }

    private var canSend: Bool {
        store.isConnected && !draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func send() {
        guard canSend else { return }
        if store.send(draft.text) {
            draft.sent()
            scrollRequest += 1
        }
    }
}

/// A native input may report one final edit after sending; retain the input identity with each edit.
struct ComposerDraft {
    private(set) var text = ""
    private(set) var inputID = UUID()

    mutating func edit(_ value: String, from inputID: UUID) {
        guard inputID == self.inputID else { return }
        text = value
    }

    mutating func sent() {
        text = ""
        inputID = UUID()
    }
}

struct WorkingRow: View {
    @EnvironmentObject var store: LoopMeStore
    var body: some View {
        HStack(spacing: 8) {
            ProgressView().controlSize(.small)
            Text(store.status.currentRun?.title.isEmpty == false ? "正在处理：\(store.status.currentRun!.title)" : "正在处理…")
                .font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
            Spacer()
            Button("停止") { store.stopRun() }.font(.caption.weight(.semibold)).buttonStyle(.plain).foregroundStyle(Theme.text)
        }
    }
}

public struct MessageRow: View {
    @EnvironmentObject var store: LoopMeStore
    let message: Message
    /// Long replies start open when true (new in this visit), folded otherwise.
    var latest: Bool
    var onSelectText: ((Message) -> Void)?
    var onReply: ((Message) -> Void)?
    @State private var expanded: Bool?

    public init(message: Message, latest: Bool = false, onSelectText: ((Message) -> Void)? = nil, onReply: ((Message) -> Void)? = nil) {
        self.message = message
        self.latest = latest
        self.onSelectText = onSelectText
        self.onReply = onReply
    }

    /// Lines past which a reply folds.
    static let foldLines = 6

    private var isLong: Bool {
        message.text.split(separator: "\n", omittingEmptySubsequences: false).count > Self.foldLines + 2 || message.text.count > 360
    }

    private var isExpanded: Bool { message.streaming || !isLong || (expanded ?? latest) }

    public var body: some View {
        switch message.role {
        case "user":
            HStack {
                Spacer(minLength: 48)
                UserBubble(text: message.text, quote: store.message(message.replyTo).map { $0.offer?.title ?? $0.text })
                    .contextMenu { menu(feedback: false) }
            }
        case "agent":
            VStack(alignment: .leading, spacing: 6) {
                if let offer = message.offer {
                    OfferCard(message: message, offer: offer)
                        .contextMenu { menu(feedback: true) }
                } else if !message.text.isEmpty {
                    HStack(spacing: 0) {
                        agentBlock
                        Spacer(minLength: 36)
                    }
                }
                if let f = message.feedback {
                    Text(f == "up" ? "👍 你觉得有用" : "👎 你觉得没用，以后少发这类")
                        .font(.caption2).foregroundStyle(Theme.secondary)
                        .padding(.leading, 6)
                }
                attachments
            }
        default:
            if message.decisionId != nil || message.itemId != nil {
                attachments
            } else {
                Text(message.text)
                    .font(.caption)
                    .foregroundStyle(Theme.secondary)
                    .frame(maxWidth: .infinity, alignment: .center)
            }
        }
    }

    /// The agent's reply as a grey block; long ones fold to a few lines with 展开.
    private var agentBlock: some View {
        VStack(alignment: .leading, spacing: 8) {
            (Text(markdown(message.text)) + Text(message.streaming ? " ▍" : "").foregroundColor(Theme.secondary))
                .font(.body)
                .foregroundStyle(Theme.text)
                .lineSpacing(2)
                .lineLimit(isExpanded ? nil : Self.foldLines)
                .fixedSize(horizontal: false, vertical: true)
            if isLong && !message.streaming {
                Button {
                    withAnimation(.easeOut(duration: 0.2)) { expanded = !isExpanded }
                } label: {
                    HStack(spacing: 4) {
                        Text(isExpanded ? "收起" : "展开全文")
                        Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                    }
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Theme.secondary)
                }
                .buttonStyle(.plain)
                .accessibilityHint(isExpanded ? "收起这条回复" : "显示这条回复的全文")
            }
        }
        .padding(.horizontal, 14).padding(.vertical, 11)
        .background(Theme.agentBubble, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
        .contentShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
        .contextMenu { menu(feedback: true) }
    }

    /// Long press: 👍/👎 (agent messages), 复制, 选择文本, 回复.
    @ViewBuilder private func menu(feedback: Bool) -> some View {
        if feedback && !message.streaming {
            ControlGroup {
                Button { store.feedback(message, "up") } label: { Label("有用", systemImage: message.feedback == "up" ? "hand.thumbsup.fill" : "hand.thumbsup") }
                Button { store.feedback(message, "down") } label: { Label("没用", systemImage: message.feedback == "down" ? "hand.thumbsdown.fill" : "hand.thumbsdown") }
            }
        }
        Button { Clipboard.copy(copyText) } label: { Label("复制", systemImage: "doc.on.doc") }
        if let onSelectText {
            Button { onSelectText(message) } label: { Label("选择文本", systemImage: "text.cursor") }
        }
        if let onReply, !message.streaming {
            Button { onReply(message) } label: { Label("回复", systemImage: "arrowshape.turn.up.left") }
        }
    }

    private var copyText: String {
        guard let o = message.offer else { return message.text }
        return [o.title, o.detail].filter { !$0.isEmpty }.joined(separator: "\n")
    }

    @ViewBuilder private var attachments: some View {
        if let did = message.decisionId, let d = store.decisions[did] {
            DecisionCard(decision: d)
        } else if let iid = message.itemId, let i = store.items[iid] {
            ItemCard(item: i)
        }
    }

    private func markdown(_ s: String) -> AttributedString {
        (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(s)
    }
}

enum ChatRow: Identifiable {
    case message(Message, key: String)
    case outgoing(LoopMeStore.OutgoingMessage)

    var id: String {
        switch self {
        case .message(_, let key): return key
        case .outgoing(let o): return o.id
        }
    }

    var role: String {
        switch self {
        case .message(let m, _): return m.role
        case .outgoing: return "user"
        }
    }
}

/// The user's own message: right-aligned blue bubble. A message being sent looks the same as a delivered one,
/// so nothing moves when the Mac confirms it.
struct UserBubble: View {
    let text: String
    var quote: String?

    var body: some View {
        VStack(alignment: .trailing, spacing: 4) {
            if let quote { QuoteLine(text: quote) }
            Text(text)
                .padding(.horizontal, 14).padding(.vertical, 10)
                .foregroundStyle(Theme.userText)
                .tint(Theme.userText)
                .background(Theme.userBubble, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
        }
    }
}

/// The quoted message above a reply.
struct QuoteLine: View {
    let text: String
    var body: some View {
        Text(text)
            .font(.caption)
            .foregroundStyle(Theme.secondary)
            .lineLimit(1)
            .padding(.leading, 8)
            .overlay(alignment: .leading) { RoundedRectangle(cornerRadius: 1).fill(Theme.accent).frame(width: 2) }
            .frame(maxWidth: 260, alignment: .trailing)
    }
}

/// 选择文本: the message in a sheet where any part can be selected.
struct SelectTextSheet: View {
    let text: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                Text(text)
                    .textSelection(.enabled)
                    .foregroundStyle(Theme.text)
                    .padding()
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .background(Theme.background)
            .navigationTitle("选择文本")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("完成") { dismiss() } }
            }
        }
        #if os(macOS)
        .frame(minWidth: 420, minHeight: 300)
        #endif
    }
}

public enum Clipboard {
    public static func copy(_ s: String) {
        #if canImport(UIKit)
        UIPasteboard.general.string = s
        #elseif canImport(AppKit)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(s, forType: .string)
        #endif
    }
}
