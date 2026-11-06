import SwiftUI
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

/// The single conversation stream with item and decision cards.
public struct ChatView: View {
    @EnvironmentObject var store: LoopMeStore
    @State private var draft = ""
    @State private var nowExpanded = false
    @State private var selecting: Message?
    @FocusState private var inputFocused: Bool

    public init() {}

    public var body: some View {
        VStack(spacing: 0) {
            connectionBanner
            NowBar(expanded: $nowExpanded)
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 14) {
                        if store.messages.isEmpty && store.outgoing.isEmpty {
                            emptyState
                        }
                        ForEach(store.messages) { m in
                            if isShown(m) {
                                MessageRow(message: m, onSelectText: { selecting = $0 }, onReply: { reply(to: $0) }).id(m.id)
                            }
                        }
                        ForEach(store.outgoing) { o in
                            HStack {
                                Spacer(minLength: 48)
                                VStack(alignment: .trailing, spacing: 3) {
                                    if let q = store.message(o.replyTo) { QuoteLine(text: q.text) }
                                    Text(o.text)
                                        .padding(.horizontal, 14).padding(.vertical, 10)
                                        .foregroundStyle(Theme.text)
                                        .background(Theme.userBubble.opacity(0.6), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                                    Text("已发送").font(.caption2).foregroundStyle(Theme.secondary)
                                }
                            }
                            .id(o.id)
                        }
                        if store.isWorking && store.status.currentRun?.messageId == nil && !nowExpanded {
                            WorkingRow().id("working")
                        }
                        Color.clear.frame(height: 4).id("bottom")
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                }
                .background(Theme.background)
                #if os(iOS)
                .scrollDismissesKeyboard(.interactively)
                #endif
                .onAppear { proxy.scrollTo("bottom", anchor: .bottom) }
                .onChange(of: store.messages.last?.text) { _, _ in
                    withAnimation(.easeOut(duration: 0.15)) { proxy.scrollTo("bottom", anchor: .bottom) }
                }
                .onChange(of: store.messages.count) { _, _ in
                    withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
                }
                .onChange(of: store.outgoing.count) { _, _ in
                    withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
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
        .onChange(of: store.nowSummary == nil) { _, empty in
            if empty { nowExpanded = false }
        }
        .sheet(item: $selecting) { m in
            SelectTextSheet(text: m.text)
        }
    }

    /// Room for the connection banner and the "现在" bar above the expanded panel.
    private var nowBarHeight: CGFloat { store.isConnected ? 56 : 92 }

    /// Suggestions come one at a time; the others wait behind 换一个.
    private func isShown(_ m: Message) -> Bool {
        guard m.role == "system", let iid = m.itemId, let item = store.items[iid], item.status == "suggested" else { return true }
        return store.currentSuggestion?.id == item.id
    }

    private func reply(to m: Message) {
        store.replyTo = m
        inputFocused = true
    }

    @ViewBuilder private var connectionBanner: some View {
        if !store.isConnected {
            HStack(spacing: 6) {
                if store.connection == .connecting { ProgressView().controlSize(.mini) }
                Text(store.connection == .connecting ? "正在连接 Mac…" : "Mac 不可达，正在重试。草稿会保留。")
                    .font(.caption)
                    .lineLimit(1)
                Spacer()
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
        HStack(alignment: .bottom, spacing: 10) {
            TextField("和 Agent 说点什么", text: $draft, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...6)
                .focused($inputFocused)
                .padding(.horizontal, 14).padding(.vertical, 10)
                .foregroundStyle(Theme.text)
                .background(Theme.card, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
                .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).stroke(Theme.border, lineWidth: 1))
                .onSubmit(send)
            if store.isWorking && draft.isEmpty {
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
        store.isConnected && !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func send() {
        guard canSend else { return }
        if store.send(draft) { draft = "" }
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
    var onSelectText: ((Message) -> Void)?
    var onReply: ((Message) -> Void)?

    public init(message: Message, onSelectText: ((Message) -> Void)? = nil, onReply: ((Message) -> Void)? = nil) {
        self.message = message
        self.onSelectText = onSelectText
        self.onReply = onReply
    }

    public var body: some View {
        switch message.role {
        case "user":
            HStack {
                Spacer(minLength: 48)
                VStack(alignment: .trailing, spacing: 4) {
                    if let q = store.message(message.replyTo) { QuoteLine(text: q.offer?.title ?? q.text) }
                    Text(message.text)
                        .padding(.horizontal, 14).padding(.vertical, 10)
                        .foregroundStyle(Theme.text)
                        .background(Theme.userBubble, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                        .contextMenu { menu(feedback: false) }
                }
            }
        case "agent":
            VStack(alignment: .leading, spacing: 6) {
                if let offer = message.offer {
                    OfferCard(message: message, offer: offer)
                        .contextMenu { menu(feedback: true) }
                } else if !message.text.isEmpty {
                    (Text(markdown(message.text)) + Text(message.streaming ? " ▍" : "").foregroundColor(Theme.secondary))
                        .font(.body)
                        .foregroundStyle(Theme.text)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                        .contextMenu { menu(feedback: true) }
                }
                if let f = message.feedback {
                    Text(f == "up" ? "👍 你觉得有用" : "👎 你觉得没用，以后少发这类")
                        .font(.caption2).foregroundStyle(Theme.secondary)
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
