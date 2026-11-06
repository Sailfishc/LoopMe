import SwiftUI
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

/// Colors from the approved prototype (dark, minimal), with light variants.
public enum Theme {
    public static let background = Color.dynamic(light: 0xF6F6F8, dark: 0x0B0B0D)
    public static let card = Color.dynamic(light: 0xFFFFFF, dark: 0x17171A)
    public static let border = Color.dynamic(light: 0xE3E3E8, dark: 0x26262B)
    public static let text = Color.dynamic(light: 0x111114, dark: 0xF2F2F5)
    public static let secondary = Color.dynamic(light: 0x6B6B73, dark: 0x9A9AA2)
    public static let userBubble = Color.dynamic(light: 0xE9E9EE, dark: 0x232328)
    public static let green = Color(hex: 0x34C77B)
    public static let accent = Color.dynamic(light: 0x4B5BD6, dark: 0xA9B4FF)
    public static let warning = Color(hex: 0xF5C46B)
    public static let danger = Color(hex: 0xFF6B6B)
    /// Pill buttons: light fill with dark text in dark mode.
    public static let pill = Color.dynamic(light: 0x111114, dark: 0xF2F2F5)
    public static let pillText = Color.dynamic(light: 0xFFFFFF, dark: 0x0B0B0D)
}

public extension Color {
    init(hex: UInt32, alpha: Double = 1) {
        self.init(.sRGB,
                  red: Double((hex >> 16) & 0xFF) / 255,
                  green: Double((hex >> 8) & 0xFF) / 255,
                  blue: Double(hex & 0xFF) / 255,
                  opacity: alpha)
    }

    static func dynamic(light: UInt32, dark: UInt32) -> Color {
        #if canImport(UIKit)
        return Color(UIColor { traits in
            UIColor(Color(hex: traits.userInterfaceStyle == .dark ? dark : light))
        })
        #elseif canImport(AppKit)
        return Color(NSColor(name: nil) { appearance in
            let isDark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            return NSColor(Color(hex: isDark ? dark : light))
        })
        #else
        return Color(hex: dark)
        #endif
    }
}

/// `appearance` app setting: `system`, `dark` (default) or `light`.
public enum AppearanceSetting {
    public static let key = "appearance"
    public static func colorScheme(_ value: String) -> ColorScheme? {
        switch value {
        case "light": return .light
        case "system": return nil
        default: return .dark
        }
    }
}

public struct PillButtonStyle: ButtonStyle {
    var prominent: Bool
    public init(prominent: Bool = true) { self.prominent = prominent }
    public func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.subheadline.weight(.semibold))
            .padding(.horizontal, 16)
            .padding(.vertical, 9)
            .foregroundStyle(prominent ? Theme.pillText : Theme.text)
            .background(prominent ? Theme.pill : Theme.card, in: Capsule())
            .overlay(Capsule().stroke(prominent ? Color.clear : Theme.border, lineWidth: 1))
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

public struct CardBackground: ViewModifier {
    public func body(content: Content) -> some View {
        content
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.card, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).stroke(Theme.border, lineWidth: 1))
    }
}

public extension View {
    func loopmeCard() -> some View { modifier(CardBackground()) }
}

public enum Fmt {
    public static func time(_ ms: Int64) -> String {
        let d = ms.date
        let f = DateFormatter()
        f.locale = Locale(identifier: "zh_CN")
        if Calendar.current.isDateInToday(d) {
            f.dateFormat = "今天 HH:mm"
        } else if Calendar.current.isDateInTomorrow(d) {
            f.dateFormat = "明天 HH:mm"
        } else {
            f.dateFormat = "M月d日 HH:mm"
        }
        return f.string(from: d)
    }

    public static func relative(_ ms: Int64) -> String {
        let f = RelativeDateTimeFormatter()
        f.locale = Locale(identifier: "zh_CN")
        f.unitsStyle = .short
        return f.localizedString(for: ms.date, relativeTo: Date())
    }

    public static func itemStatus(_ s: String) -> String {
        switch s {
        case "scheduled": return "已安排"
        case "active": return "进行中"
        case "needsYou": return "等你决定"
        case "snoozed": return "稍后提醒"
        case "done": return "已完成"
        case "dropped": return "不再跟进"
        case "suggested": return "建议跟进"
        default: return s
        }
    }

    public static func sessionState(_ s: String) -> String {
        switch s {
        case "working": return "正在跑"
        case "waitingApproval": return "等授权"
        case "ended": return "已结束"
        default: return "停下了"
        }
    }

    public static func itemColor(_ s: String) -> Color {
        switch s {
        case "active": return Theme.green
        case "needsYou": return Theme.warning
        case "suggested": return Theme.accent
        case "done": return Theme.secondary
        case "dropped": return Theme.secondary
        default: return Theme.accent
        }
    }

    public static func connection(_ s: LoopMeConnection.State) -> String {
        switch s {
        case .idle: return "未连接"
        case .connecting: return "连接中…"
        case .connected: return "已连接"
        case .failed(let e): return "不可达：\(e)"
        }
    }

    public static func permission(_ s: String) -> String {
        switch s {
        case "readOnly": return "只读"
        case "full": return "完全访问"
        default: return "自动 · 仅工作区"
        }
    }

    /// Minutes until the next 09:00 local time.
    public static func minutesUntilTomorrowMorning() -> Int {
        let cal = Calendar.current
        var c = cal.dateComponents([.year, .month, .day], from: Date())
        c.hour = 9
        c.minute = 0
        var target = cal.date(from: c) ?? Date()
        if target <= Date() { target = cal.date(byAdding: .day, value: 1, to: target) ?? target }
        return max(1, Int(target.timeIntervalSinceNow / 60))
    }
}
