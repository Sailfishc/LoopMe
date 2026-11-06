import SwiftUI

public struct ItemCard: View {
    @EnvironmentObject var store: LoopMeStore
    let item: Item

    public init(item: Item) { self.item = item }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Circle().fill(Fmt.itemColor(item.status)).frame(width: 8, height: 8)
                Text(item.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                Spacer(minLength: 4)
                Text(item.status == "suggested" ? suggestionLabel : Fmt.itemStatus(item.status))
                    .font(.caption2.weight(.medium))
                    .padding(.horizontal, 8).padding(.vertical, 3)
                    .foregroundStyle(Fmt.itemColor(item.status))
                    .background(Fmt.itemColor(item.status).opacity(0.14), in: Capsule())
            }
            if !item.detail.isEmpty {
                Text(item.detail).font(.footnote).foregroundStyle(Theme.secondary).lineLimit(3)
            }
            metaLine
            actions
        }
        .loopmeCard()
    }

    private var suggestionLabel: String {
        let all = store.openSuggestions
        guard let i = all.firstIndex(where: { $0.id == item.id }), all.count > 1 else { return "建议" }
        return "建议 · \(i + 1)/\(all.count)"
    }

    @ViewBuilder private var metaLine: some View {
        let parts: [String] = [
            item.doneWhen.map { "完成条件：\($0)" },
            item.checks > 0 ? "已检查 \(item.checks) 次" : nil,
        ].compactMap { $0 }
        if !parts.isEmpty {
            Text(parts.joined(separator: " · ")).font(.caption).foregroundStyle(Theme.secondary)
        }
    }

    @ViewBuilder private var actions: some View {
        if item.status == "suggested" {
            let all = store.openSuggestions
            HStack(spacing: 8) {
                Button("盯着它") { store.itemAction(item, "follow") }.buttonStyle(PillButtonStyle())
                if all.count > 1 {
                    Button("换一个") { store.nextSuggestion() }.buttonStyle(PillButtonStyle(prominent: false))
                }
                Button("以后再说") { store.itemAction(item, "ignore") }
                    .buttonStyle(.plain)
                    .font(.subheadline)
                    .foregroundStyle(Theme.secondary)
            }
            .disabled(!store.isConnected)
        } else if item.isOpen {
            HStack(spacing: 8) {
                Button("现在检查") { store.itemAction(item, "checkNow") }
                    .buttonStyle(PillButtonStyle(prominent: false))
                Menu {
                    Button("1 小时后提醒") { store.itemAction(item, "snooze", minutes: 60) }
                    Button("3 小时后提醒") { store.itemAction(item, "snooze", minutes: 180) }
                    Button("明早提醒") { store.itemAction(item, "snooze", minutes: Fmt.minutesUntilTomorrowMorning()) }
                    Divider()
                    Button("标记完成") { store.itemAction(item, "done") }
                    Button("不再跟进", role: .destructive) { store.itemAction(item, "drop") }
                } label: {
                    Text("更多").font(.subheadline.weight(.semibold))
                        .padding(.horizontal, 16).padding(.vertical, 9)
                        .foregroundStyle(Theme.text)
                        .background(Theme.card, in: Capsule())
                        .overlay(Capsule().stroke(Theme.border, lineWidth: 1))
                }
                .menuStyle(.button)
                .buttonStyle(.plain)
                .fixedSize()
            }
            .disabled(!store.isConnected)
        }
    }
}

public struct DecisionCard: View {
    @EnvironmentObject var store: LoopMeStore
    let decision: Decision

    public init(decision: Decision) { self.decision = decision }

    private var kindLabel: String {
        switch decision.kind {
        case "approval": return "等你批准"
        case "sessionApproval": return "你的 Codex 在等授权"
        case "continue": return "要不要继续跟进"
        default: return "等你决定"
        }
    }

    @State private var showAllDetail = false

    private var isApproval: Bool { decision.kind.hasSuffix("pproval") }
    private var pending: Bool { decision.status == "pending" }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Image(systemName: isApproval ? "lock.shield.fill" : "hand.raised.fill")
                Text(kindLabel)
                Spacer(minLength: 8)
                if pending && decision.kind != "sessionApproval" {
                    Text("\(Fmt.relative(decision.expiresAt))过期").foregroundStyle(Theme.secondary).fontWeight(.regular)
                }
            }
            .font(.caption.weight(.semibold))
            .foregroundStyle(pending ? Theme.warning : Theme.secondary)
            Text(decision.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
            if !decision.detail.isEmpty { detail }
            footer
        }
        .loopmeCard()
        .overlay(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .stroke(pending ? Theme.warning.opacity(0.45) : Color.clear, lineWidth: 1)
        )
    }

    /// Commands and paths in a quiet box, folded after six lines.
    @ViewBuilder private var detail: some View {
        let long = decision.detail.split(separator: "\n", omittingEmptySubsequences: false).count > 6 || decision.detail.count > 320
        VStack(alignment: .leading, spacing: 6) {
            Text(decision.detail)
                .font(isApproval ? .system(.footnote, design: .monospaced) : .footnote)
                .foregroundStyle(Theme.secondary)
                .lineLimit(showAllDetail || !long ? nil : 6)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
            if long {
                Button(showAllDetail ? "收起" : "展开全部") { withAnimation(.easeOut(duration: 0.2)) { showAllDetail.toggle() } }
                    .font(.caption.weight(.semibold))
                    .buttonStyle(.plain)
                    .foregroundStyle(Theme.accent)
            }
        }
        .padding(isApproval ? 10 : 0)
        .background(isApproval ? Theme.subtleFill : Color.clear, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
    }

    private var resultText: String {
        switch decision.status {
        case "approved": return "已同意"
        case "rejected": return "已拒绝"
        default: return decision.kind == "sessionApproval" ? "交回 Mac 上处理" : "已过期，按拒绝处理"
        }
    }

    @ViewBuilder private var footer: some View {
        if pending {
            DecisionButtons(decision: decision)
        } else {
            Label(resultText, systemImage: decision.status == "approved" ? "checkmark.circle.fill" : "xmark.circle")
                .font(.caption.weight(.medium))
                .foregroundStyle(decision.status == "approved" ? Theme.green : Theme.secondary)
                .padding(.horizontal, 10).padding(.vertical, 5)
                .background(Theme.subtleFill, in: Capsule())
        }
    }
}

/// 拒绝 / 同意 as two equal buttons (and 回 Mac 再说 under them for the user's own Codex), with the in-flight state.
public struct DecisionButtons: View {
    @EnvironmentObject var store: LoopMeStore
    let decision: Decision
    @State private var tapped = 0

    public init(decision: Decision, showExpiry: Bool = false) {
        self.decision = decision
    }

    private func label(_ choice: String) -> String {
        switch choice {
        case "approve": return "同意"
        case "reject": return "拒绝"
        default: return "回 Mac 再说"
        }
    }

    private func answer(_ choice: String) {
        tapped += 1
        store.answer(decision, choice: choice)
    }

    public var body: some View {
        Group {
            if let choice = store.answering[decision.id] {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("已选「\(label(choice))」，等 Mac 确认").font(.caption).foregroundStyle(Theme.secondary)
                }
                .frame(maxWidth: .infinity, minHeight: 40)
                .background(Theme.subtleFill, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            } else {
                VStack(spacing: 8) {
                    HStack(spacing: 10) {
                        Button { answer("reject") } label: { Label("拒绝", systemImage: "xmark") }
                            .buttonStyle(ActionButtonStyle(.secondary))
                        Button { answer("approve") } label: { Label("同意", systemImage: "checkmark") }
                            .buttonStyle(ActionButtonStyle(.primary))
                    }
                    if decision.kind == "sessionApproval" {
                        Button { answer("later") } label: {
                            Label("回 Mac 再说", systemImage: "desktopcomputer").font(.caption.weight(.medium))
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(Theme.secondary)
                        .frame(maxWidth: .infinity)
                        .padding(.top, 2)
                    }
                    if !store.isConnected {
                        Text("Mac 不可达，暂时不能回答").font(.caption2).foregroundStyle(Theme.secondary)
                    }
                }
                .disabled(!store.isConnected)
                .opacity(store.isConnected ? 1 : 0.5)
            }
        }
        #if os(iOS)
        .sensoryFeedback(.impact(weight: .medium), trigger: tapped)
        #endif
    }
}

/// A proactive message as a card the user can act on: 让我来 / 我自己来 / 别管它,
/// or, after a takeover, 看改动 / 推送 / 撤销.
public struct OfferCard: View {
    @EnvironmentObject var store: LoopMeStore
    let message: Message
    let offer: Offer
    @State private var showWhy = false
    @State private var diff: String?
    @State private var confirmPush = false

    public init(message: Message, offer: Offer) {
        self.message = message
        self.offer = offer
    }

    private var labelColor: Color {
        switch offer.kind {
        case "result": return Theme.green
        case "takeover": return Theme.accent
        default: return Theme.secondary
        }
    }

    private var runForThis: RunInfo? {
        guard let r = store.status.currentRun, r.messageId == message.id else { return nil }
        return r
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(offer.label).font(.caption.weight(.semibold)).foregroundStyle(labelColor)
            Text(offer.title).font(.body.weight(.semibold)).foregroundStyle(Theme.text)
            if !offer.detail.isEmpty {
                Text(offer.detail).font(.subheadline).foregroundStyle(Theme.secondary).textSelection(.enabled)
            }
            if let why = offer.why, !why.isEmpty {
                Button {
                    withAnimation(.easeOut(duration: 0.15)) { showWhy.toggle() }
                } label: {
                    Label(showWhy ? "收起依据" : (offer.kind == "result" ? "看改了哪些文件" : "看依据"), systemImage: showWhy ? "chevron.up" : "chevron.down")
                        .font(.caption)
                        .foregroundStyle(Theme.secondary)
                }
                .buttonStyle(.plain)
                if showWhy {
                    Text(why)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(Theme.secondary)
                        .textSelection(.enabled)
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Theme.background, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                }
            }
            footer
        }
        .loopmeCard()
        .sheet(isPresented: Binding(get: { diff != nil }, set: { if !$0 { diff = nil } })) {
            DiffSheet(title: offer.title, branch: offer.branch, diff: diff ?? "")
        }
        .confirmationDialog("推送到 origin/\(offer.branch ?? "")？", isPresented: $confirmPush, titleVisibility: .visible) {
            Button("推送") { store.offerAct(message, "push") }
            Button("取消", role: .cancel) {}
        } message: {
            Text("推送后别人就能看到这个分支。不会合并，也不会改你本地的分支。")
        }
    }

    @ViewBuilder private var footer: some View {
        if offer.state == "working" {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text(runForThis?.step ?? "排队中…").font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                Spacer()
                Button("停止") { store.offerAct(message, "stop") }
                    .font(.caption.weight(.semibold))
                    .buttonStyle(.plain)
                    .foregroundStyle(Theme.danger)
            }
        } else if let action = store.acting[message.id] {
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text("已发送：\(offer.actions.first { $0.id == action }?.label ?? action)").font(.caption).foregroundStyle(Theme.secondary)
            }
        } else if offer.state == "open" && !offer.actions.isEmpty {
            HStack(spacing: 8) {
                ForEach(offer.actions) { a in
                    actionButton(a)
                }
            }
            .disabled(!store.isConnected)
        }
        if let note = offer.note, offer.state != "open" || offer.kind != "result" {
            Text(note).font(.caption).foregroundStyle(Theme.secondary)
        }
    }

    @ViewBuilder private func actionButton(_ a: OfferAction) -> some View {
        let tap: () -> Void = {
            switch a.id {
            case "diff": Task { @MainActor in diff = await store.offerDiff(message) }
            case "push": confirmPush = true
            default: store.offerAct(message, a.id)
            }
        }
        switch a.style {
        case "primary": Button(a.label, action: tap).buttonStyle(PillButtonStyle())
        case "secondary": Button(a.label, action: tap).buttonStyle(PillButtonStyle(prominent: false))
        default:
            Button(a.label, action: tap).buttonStyle(.plain).font(.subheadline).foregroundStyle(Theme.secondary)
        }
    }
}

struct DiffSheet: View {
    let title: String
    let branch: String?
    let diff: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView([.vertical, .horizontal]) {
                Text(diff.isEmpty ? "没有改动" : diff)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .padding()
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .background(Theme.background)
            .navigationTitle(branch ?? title)
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("完成") { dismiss() } }
            }
        }
        #if os(macOS)
        .frame(minWidth: 560, minHeight: 480)
        #endif
    }
}
