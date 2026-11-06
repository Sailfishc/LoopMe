import SwiftUI

/// What the agent is up to, for the "现在" bar (iOS/Mac chat) and the Mac menu bar.
/// Priority: 等你决定 > 正在做 > 在盯.
public struct NowSummary: Equatable {
    public enum Kind { case decide, working, watching }
    public var kind: Kind
    public var label: String
    public var text: String

    /// Short form for the Mac menu bar.
    public var menuBarText: String {
        switch kind {
        case .decide: return "等你决定"
        case .working: return "正在做"
        case .watching: return ""
        }
    }
}

public struct NowRow: Identifiable, Equatable {
    public var id: String
    public var title: String
    public var detail: String
}

public extension LoopMeStore {
    /// Items and sessions LoopMe keeps an eye on, as one-line rows.
    var watchingRows: [NowRow] {
        let items = openItems
            .filter { $0.status != "needsYou" }
            .map { NowRow(id: $0.id, title: $0.title, detail: Fmt.itemStatus($0.status)) }
        let sessions = watchedSessions.map { s in
            NowRow(id: "s:" + s.id, title: "你的 Codex：\(s.title)", detail: Fmt.sessionState(s.state))
        }
        return items + sessions
    }

    var nowSummary: NowSummary? {
        if let d = pendingDecisions.first {
            let more = pendingDecisions.count > 1 ? "（还有 \(pendingDecisions.count - 1) 件）" : ""
            return NowSummary(kind: .decide, label: "等你决定：", text: d.title + more)
        }
        if isWorking, let run = status.currentRun {
            let step = run.step.map { " · \($0)" } ?? ""
            return NowSummary(kind: .working, label: "正在做：", text: run.title + step)
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
        case .working: return Theme.green
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
            .fill(kind == .decide ? Theme.warning : (kind == .working ? Theme.green : Theme.secondary))
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

    public init(onClose: (() -> Void)? = nil) { self.onClose = onClose }

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
