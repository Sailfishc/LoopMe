import SwiftUI

/// The single conversation stream with item and decision cards.
public struct ChatView: View {
    @EnvironmentObject var store: LoopMeStore
    @State private var draft = ""
    @FocusState private var inputFocused: Bool

    public init() {}

    public var body: some View {
        VStack(spacing: 0) {
            connectionBanner
            needsYouBanner
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 14) {
                        if store.messages.isEmpty && store.outgoing.isEmpty {
                            emptyState
                        }
                        ForEach(store.messages) { m in
                            MessageRow(message: m).id(m.id)
                        }
                        ForEach(store.outgoing) { o in
                            HStack {
                                Spacer(minLength: 48)
                                VStack(alignment: .trailing, spacing: 3) {
                                    Text(o.text)
                                        .padding(.horizontal, 14).padding(.vertical, 10)
                                        .foregroundStyle(Theme.text)
                                        .background(Theme.userBubble.opacity(0.6), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                                    Text("已发送").font(.caption2).foregroundStyle(Theme.secondary)
                                }
                            }
                            .id(o.id)
                        }
                        if store.isWorking {
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

    @ViewBuilder private var needsYouBanner: some View {
        let n = store.pendingDecisions.count
        if n > 0 {
            HStack(spacing: 8) {
                Circle().fill(Theme.warning).frame(width: 8, height: 8)
                Text("有 \(n) 件事需要你决定").font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                Spacer()
            }
            .padding(.horizontal, 16).padding(.vertical, 10)
            .background(Theme.warning.opacity(0.12))
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
        .padding(.horizontal, 12).padding(.vertical, 10)
        .background(Theme.background)
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

    public init(message: Message) { self.message = message }

    public var body: some View {
        switch message.role {
        case "user":
            HStack {
                Spacer(minLength: 48)
                Text(message.text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 14).padding(.vertical, 10)
                    .foregroundStyle(Theme.text)
                    .background(Theme.userBubble, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            }
        case "agent":
            VStack(alignment: .leading, spacing: 10) {
                if !message.text.isEmpty {
                    (Text(markdown(message.text)) + Text(message.streaming ? " ▍" : "").foregroundColor(Theme.secondary))
                        .font(.body)
                        .foregroundStyle(Theme.text)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
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
