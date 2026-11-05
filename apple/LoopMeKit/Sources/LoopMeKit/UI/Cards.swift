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
                Text(Fmt.itemStatus(item.status))
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

    @ViewBuilder private var metaLine: some View {
        let parts: [String] = [
            item.nextCheckAt.map { "下次检查 \(Fmt.time($0))" },
            item.doneWhen.map { "完成条件：\($0)" },
            item.checks > 0 ? "已检查 \(item.checks) 次" : nil,
        ].compactMap { $0 }
        if !parts.isEmpty {
            Text(parts.joined(separator: " · ")).font(.caption).foregroundStyle(Theme.secondary)
        }
    }

    @ViewBuilder private var actions: some View {
        if item.status == "suggested" {
            HStack(spacing: 8) {
                Button("跟进") { store.itemAction(item, "follow") }.buttonStyle(PillButtonStyle())
                Button("忽略") { store.itemAction(item, "ignore") }.buttonStyle(PillButtonStyle(prominent: false))
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
        case "approval": return "需要你批准"
        case "continue": return "是否继续跟进"
        default: return "需要你决定"
        }
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(kindLabel, systemImage: "hand.raised.fill")
                .font(.caption.weight(.semibold))
                .foregroundStyle(decision.status == "pending" ? Theme.warning : Theme.secondary)
            Text(decision.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
            if !decision.detail.isEmpty {
                Text(decision.detail)
                    .font(decision.kind == "approval" ? .system(.footnote, design: .monospaced) : .footnote)
                    .foregroundStyle(Theme.secondary)
                    .textSelection(.enabled)
            }
            footer
        }
        .loopmeCard()
        .overlay(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .stroke(decision.status == "pending" ? Theme.warning.opacity(0.5) : Color.clear, lineWidth: 1)
        )
    }

    private var resultText: String {
        switch decision.status {
        case "approved": return "已同意 · 已完成"
        case "rejected": return "已拒绝 · 已完成"
        default: return "已过期，按拒绝处理"
        }
    }

    @ViewBuilder private var footer: some View {
        if decision.status == "pending" {
            if let choice = store.answering[decision.id] {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.small)
                    Text("已发送：\(choice == "approve" ? "同意" : "拒绝")，等待 Mac 确认")
                        .font(.caption).foregroundStyle(Theme.secondary)
                }
            } else {
                HStack(spacing: 8) {
                    Button("同意") { store.answer(decision, approve: true) }.buttonStyle(PillButtonStyle())
                    Button("拒绝") { store.answer(decision, approve: false) }.buttonStyle(PillButtonStyle(prominent: false))
                    Spacer()
                    Text("\(Fmt.relative(decision.expiresAt))过期").font(.caption2).foregroundStyle(Theme.secondary)
                }
                .disabled(!store.isConnected)
                if !store.isConnected {
                    Text("Mac 不可达，暂时不能回答").font(.caption2).foregroundStyle(Theme.secondary)
                }
            }
        } else {
            Label(resultText, systemImage: decision.status == "approved" ? "checkmark.circle.fill" : "xmark.circle")
                .font(.caption.weight(.medium))
                .foregroundStyle(decision.status == "approved" ? Theme.green : Theme.secondary)
        }
    }
}
