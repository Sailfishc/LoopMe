import SwiftUI

/// What needs the user, for the "现在" bar (iOS/Mac chat) and the Mac menu bar.
/// Priority: 等你 (decisions + sessions that need you) > LoopMe 正在做 > 成果 > 在跑 > 在盯.
public struct NowSummary: Equatable {
    public enum Kind { case decide, working, result, watching }
    public var kind: Kind
    public var label: String
    public var text: String
    /// How many things wait for the user.
    public var waiting: Int = 0

    /// Short form for the Mac menu bar.
    public var menuBarText: String {
        switch kind {
        case .decide: return "\(waiting) 件等你"
        case .working: return "正在做"
        case .result, .watching: return ""
        }
    }
}

public struct NowRow: Identifiable, Equatable {
    public var id: String
    public var title: String
    public var detail: String
}

public extension LoopMeStore {
    /// Items LoopMe keeps an eye on, as one-line rows. (Codex sessions have their own groups.)
    var watchingRows: [NowRow] {
        openItems
            .filter { $0.status != "needsYou" }
            .map { NowRow(id: $0.id, title: $0.title, detail: Fmt.itemStatus($0.status)) }
    }

    /// Everything that waits for the user right now.
    var waitingCount: Int { pendingDecisions.count + needsYouSessions.count }

    var nowSummary: NowSummary? {
        let running = runningSessions.count
        let tail = running > 0 ? " · \(running) 个在跑" : ""
        let waiting = waitingCount
        if waiting > 0 {
            let first: String
            if let s = needsYouSessions.first, let a = s.attention {
                first = "\(a.label) \(s.projectName)"
            } else {
                first = pendingDecisions.first?.title ?? ""
            }
            return NowSummary(kind: .decide, label: "\(waiting) 件等你\(tail) · ", text: first, waiting: waiting)
        }
        if isWorking, let run = status.currentRun {
            let step = run.step.map { " · \($0)" } ?? ""
            return NowSummary(kind: .working, label: "正在做：", text: run.title + step)
        }
        let results = resultSessions
        if let s = results.first, let a = s.attention {
            return NowSummary(kind: .result, label: "\(results.count) 个成果\(tail) · ", text: "\(s.projectName) \(a.summary)")
        }
        if running > 0 {
            return NowSummary(kind: .watching, label: "\(running) 个在跑", text: " · 没有要你做的")
        }
        let rows = watchingRows
        if let first = rows.first {
            return NowSummary(kind: .watching, label: "在盯 \(rows.count) 件事 · ", text: first.title)
        }
        return nil
    }
}

/// The collapsible "现在" bar above the chat.
public struct NowBar: View {
    @EnvironmentObject var store: LoopMeStore
    @Binding var expanded: Bool

    public init(expanded: Binding<Bool>) { _expanded = expanded }

    public var body: some View {
        if let now = store.nowSummary {
            Button {
                withAnimation(.easeOut(duration: 0.2)) { expanded.toggle() }
            } label: {
                HStack(spacing: 8) {
                    NowDot(kind: now.kind)
                    (Text(now.label).fontWeight(.semibold).foregroundColor(color(now.kind)) + Text(now.text).foregroundColor(Theme.text))
                        .font(.subheadline)
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    Image(systemName: expanded ? "chevron.up" : "chevron.down")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Theme.secondary)
                }
                .padding(.horizontal, 14).padding(.vertical, 10)
                .background(background(now.kind), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).stroke(border(now.kind), lineWidth: 1))
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 16).padding(.vertical, 6)
            .accessibilityLabel("现在：\(now.label)\(now.text)")
            .accessibilityHint(expanded ? "收起" : "展开")
        }
    }

    private func color(_ k: NowSummary.Kind) -> Color {
        switch k {
        case .decide: return Theme.warning
        case .working, .result: return Theme.green
        case .watching: return Theme.secondary
        }
    }

    private func background(_ k: NowSummary.Kind) -> Color {
        k == .decide ? Theme.warning.opacity(0.12) : Theme.card
    }

    private func border(_ k: NowSummary.Kind) -> Color {
        k == .decide ? Theme.warning.opacity(0.35) : Theme.border
    }
}

struct NowDot: View {
    let kind: NowSummary.Kind
    @State private var dim = false

    var body: some View {
        Circle()
            .fill(kind == .decide ? Theme.warning : (kind == .working || kind == .result ? Theme.green : Theme.secondary))
            .frame(width: 8, height: 8)
            .opacity(kind == .working && dim ? 0.35 : 1)
            .onAppear {
                guard kind == .working else { return }
                withAnimation(.easeInOut(duration: 0.8).repeatForever(autoreverses: true)) { dim = true }
            }
    }
}

/// The expanded "现在" panel: everything that needs you, is running, or is being watched.
public struct NowPanel: View {
    @EnvironmentObject var store: LoopMeStore
    var onClose: (() -> Void)?
    @State private var runningOpen = false

    public init(onClose: (() -> Void)? = nil) { self.onClose = onClose }

    static func elapsed(since ms: Int64) -> String {
        let min = max(0, (nowMillis() - ms) / 60_000)
        if min < 1 { return "刚开始" }
        if min < 60 { return "已跑 \(min) 分钟" }
        return "已跑 \(min / 60) 小时 \(min % 60) 分"
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if !store.pendingDecisions.isEmpty {
                group("等你决定", Theme.warning) {
                    ForEach(store.pendingDecisions) { d in
                        VStack(alignment: .leading, spacing: 8) {
                            Text(d.title).font(.subheadline).foregroundStyle(Theme.text)
                            DecisionButtons(decision: d)
                        }
                    }
                }
            }
            let needs = store.needsYouSessions
            if !needs.isEmpty {
                group("等你", Theme.warning) {
                    ForEach(needs) { s in AttentionCard(session: s) }
                }
            }
            let results = store.resultSessions
            if !results.isEmpty {
                group("成果", Theme.green) {
                    ForEach(results) { s in AttentionCard(session: s) }
                }
            }
            if store.isWorking, let run = store.status.currentRun {
                group("正在做", Theme.green) {
                    HStack(alignment: .top) {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(run.title).font(.subheadline).foregroundStyle(Theme.text)
                            if let step = run.step {
                                Text(step).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                            }
                        }
                        Spacer()
                        Button("停止") { store.stopRun() }
                            .font(.caption.weight(.semibold))
                            .buttonStyle(.plain)
                            .foregroundStyle(Theme.danger)
                    }
                }
            }
            let running = store.runningSessions
            if !running.isEmpty {
                DisclosureGroup(isExpanded: $runningOpen) {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(running) { s in
                            HStack(alignment: .firstTextBaseline) {
                                Text(s.projectName).font(.subheadline).foregroundStyle(Theme.text).lineLimit(1)
                                Spacer(minLength: 8)
                                Text(Self.elapsed(since: s.turnStartedAt ?? s.updatedAt)).font(.caption).foregroundStyle(Theme.secondary)
                            }
                        }
                    }
                    .padding(.top, 6)
                } label: {
                    Text("\(running.count) 个在跑").font(.caption.weight(.semibold)).foregroundStyle(Theme.secondary)
                }
                .tint(Theme.secondary)
            }
            let rows = store.watchingRows
            if !rows.isEmpty {
                group("在盯", Theme.secondary) {
                    ForEach(rows) { r in
                        HStack(alignment: .firstTextBaseline) {
                            Text(r.title).font(.subheadline).foregroundStyle(Theme.text).lineLimit(1)
                            Spacer(minLength: 8)
                            Text(r.detail).font(.caption).foregroundStyle(Theme.secondary)
                        }
                    }
                }
            }
            if store.nowSummary == nil {
                Text("现在没有进行中的事").font(.subheadline).foregroundStyle(Theme.secondary)
            }
            if let onClose {
                Button("收起", action: onClose)
                    .font(.caption)
                    .foregroundStyle(Theme.secondary)
                    .frame(maxWidth: .infinity)
                    .buttonStyle(.plain)
            }
        }
        .padding(14)
        .background(Theme.card, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).stroke(Theme.border, lineWidth: 1))
    }

    private func group<Content: View>(_ title: String, _ color: Color, @ViewBuilder _ content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.caption.weight(.semibold)).foregroundStyle(color)
            content()
        }
    }
}

/// One session that needs the user or has a result: a label, one sentence, one line of evidence, and up to three actions.
struct AttentionCard: View {
    @EnvironmentObject var store: LoopMeStore
    let session: CodexSession

    var body: some View {
        if let a = session.attention {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 6) {
                    Text(a.label)
                        .font(.caption2.weight(.bold))
                        .padding(.horizontal, 6).padding(.vertical, 2)
                        .foregroundStyle(a.needsYou ? Color.black : Theme.pillText)
                        .background(a.needsYou ? Theme.warning : Theme.green, in: Capsule())
                    Text(session.projectName).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
                    Spacer(minLength: 4)
                    Text(Fmt.relative(a.at)).font(.caption2).foregroundStyle(Theme.secondary)
                }
                Text(a.summary).font(.subheadline).foregroundStyle(Theme.text).fixedSize(horizontal: false, vertical: true)
                if let line = evidenceLine(a) {
                    Text(line).font(.caption).foregroundStyle(Theme.secondary).lineLimit(2)
                }
                HStack(spacing: 8) {
                    if let m = takeover(a) {
                        Button("让我来") { store.offerAct(m, "take") }
                            .buttonStyle(ActionButtonStyle(.primary))
                            .disabled(!store.isConnected || store.acting[m.id] != nil)
                    }
                    Button("稍后") { store.snoozeSession(session) }
                        .buttonStyle(ActionButtonStyle(.secondary))
                    Button(a.needsYou ? "知道了" : "好的") { store.ackSession(session) }
                        .buttonStyle(ActionButtonStyle(.secondary))
                }
                .disabled(!store.isConnected)
            }
            .padding(12)
            .background(Theme.subtleFill, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .contextMenu {
                Button { Clipboard.copy("\(session.projectName)：\(a.summary)") } label: { Label("复制", systemImage: "doc.on.doc") }
                Button(role: .destructive) { store.muteSession(session) } label: { Label("这个会话先别提醒", systemImage: "bell.slash") }
            }
            .accessibilityElement(children: .contain)
        }
    }

    private func evidenceLine(_ a: Attention) -> String? {
        let parts = [a.causeText, a.evidence].compactMap { $0 }.filter { !$0.isEmpty }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    /// The takeover offer for a stuck session, while it is still open.
    private func takeover(_ a: Attention) -> Message? {
        guard let m = store.message(a.messageId), let o = m.offer, o.kind == "takeover", o.state == "open" else { return nil }
        return m
    }
}
