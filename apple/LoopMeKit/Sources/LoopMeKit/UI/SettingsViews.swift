import SwiftUI

/// Settings shared by the iPhone settings page and the Mac settings window.
/// Every control shows the Mac's confirmed value and sends a change request.
public struct SettingsForm: View {
    @EnvironmentObject var store: LoopMeStore
    @AppStorage(AppearanceSetting.key) private var appearance = "dark"
    @State private var pushReport: String?
    @State private var confirmUnpair = false
    private let onUnpair: (() -> Void)?
    private let isMac: Bool

    /// `onUnpair` adds the “解除配对” row (iPhone). `isMac` enables Mac-only fields.
    public init(isMac: Bool = false, onUnpair: (() -> Void)? = nil) {
        self.isMac = isMac
        self.onUnpair = onUnpair
    }

    public var body: some View {
        Form {
            macSection
            permissionSection
            notifySection
            chatSection
            Section("记忆与 Profile") {
                NavigationLink("记忆") { MemoryView() }
                NavigationLink("Profile") { ProfileView() }
            }
            Section("外观与语言") {
                Picker("外观", selection: $appearance) {
                    Text("深色").tag("dark")
                    Text("浅色").tag("light")
                    Text("跟随系统").tag("system")
                }
                Picker("Agent 回复语言", selection: binding({ store.profile.language }, { v in store.updateProfile(["language": v]) })) {
                    Text("跟随我的消息").tag("")
                    Text("中文").tag("中文")
                    Text("English").tag("English")
                }
            }
            if let onUnpair {
                Section {
                    Button("解除与这台 Mac 的配对", role: .destructive) { confirmUnpair = true }
                } footer: {
                    Text("解除后需要在 Mac 上重新扫码连接。")
                }
                .confirmationDialog("解除配对？", isPresented: $confirmUnpair, titleVisibility: .visible) {
                    Button("解除配对", role: .destructive, action: onUnpair)
                }
            }
        }
        .formStyle(.grouped)
        .disabled(!store.isConnected && !store.hasSynced)
    }

    private func binding<T>(_ get: @escaping () -> T, _ set: @escaping (T) -> Void) -> Binding<T> {
        Binding(get: get, set: set)
    }

    // MARK: sections

    private var macSection: some View {
        Section("Mac") {
            row("名称", store.status.macName.isEmpty ? (store.credentials?.macName ?? "Mac") : store.status.macName)
            row("连接", Fmt.connection(store.connection))
            row("Codex", codexText)
            row("Agent", agentText)
            if let t = store.status.nextWakeAt {
                row("下次主动工作", Fmt.time(t))
            }
            Toggle("暂停主动工作", isOn: binding({ store.settings.paused }, { v in store.updateSettings(["paused": v]) }))
        }
    }

    private var codexText: String {
        let c = store.status.codex
        if !c.found { return "未安装" }
        var s = c.version ?? "已安装"
        if c.loggedIn == false { s += " · 未登录" }
        return s
    }

    private var agentText: String {
        switch store.status.agent {
        case "working": return "工作中"
        case "paused": return "已暂停"
        case "error": return "出错"
        default: return "空闲"
        }
    }

    private var permissionSection: some View {
        Section {
            Picker("权限档位", selection: binding({ store.settings.permissionLevel }, { v in store.updateSettings(["permissionLevel": v]) })) {
                Text("只读").tag("readOnly")
                Text("自动 · 仅工作区").tag("workspace")
                Text("完全访问").tag("full")
            }
            if isMac {
                WorkspaceRootsEditor()
            } else if !store.settings.workspaceRoots.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    Text("授权文件夹").font(.subheadline)
                    ForEach(store.settings.workspaceRoots, id: \.self) { r in
                        Text(r).font(.caption.monospaced()).foregroundStyle(.secondary)
                    }
                }
            }
        } header: {
            Text("Agent 权限")
        } footer: {
            Text(permissionFooter)
        }
    }

    private var permissionFooter: String {
        switch store.settings.permissionLevel {
        case "readOnly": return "只读：只能查看文件和运行只读命令，任何修改都会先问你。"
        case "full": return "完全访问：不受沙箱限制，请只在信任的环境中使用。"
        default: return "自动 · 仅工作区：在授权文件夹内可以自动修改文件和运行命令，超出范围时会先问你。"
        }
    }

    private var notifySection: some View {
        Section {
            Toggle("需要我决定时", isOn: binding({ store.settings.notify.decision }, { v in store.updateSettings(["notify": ["decision": v]]) }))
            Toggle("交代的事有结果时", isOn: binding({ store.settings.notify.result }, { v in store.updateSettings(["notify": ["result": v]]) }))
            Toggle("执行失败需要我介入时", isOn: binding({ store.settings.notify.failure }, { v in store.updateSettings(["notify": ["failure": v]]) }))
            Toggle("早间汇报", isOn: binding({ store.settings.briefEnabled }, { v in store.updateSettings(["briefEnabled": v, "notify": ["brief": v]]) }))
            if store.settings.briefEnabled {
                DatePicker("汇报时间", selection: timeBinding(\.briefTime, key: "briefTime"), displayedComponents: .hourAndMinute)
                Toggle("只在工作日", isOn: binding({ store.settings.briefWeekdaysOnly }, { v in store.updateSettings(["briefWeekdaysOnly": v]) }))
            }
            DatePicker("安静时段开始", selection: timeBinding(\.quietStart, key: "quietStart"), displayedComponents: .hourAndMinute)
            DatePicker("安静时段结束", selection: timeBinding(\.quietEnd, key: "quietEnd"), displayedComponents: .hourAndMinute)
            Toggle("锁屏通知显示摘要", isOn: binding({ store.settings.lockScreenSummary }, { v in store.updateSettings(["lockScreenSummary": v]) }))
            Button("发送测试推送") {
                Task { @MainActor in pushReport = await store.testPush() }
            }
            if let pushReport {
                Text(pushReport).font(.caption).foregroundStyle(.secondary)
            }
        } header: {
            Text("什么时候通知我")
        } footer: {
            Text("同一事项每天最多提醒 \(store.settings.limits.perItemPerDay) 次，每小时最多 \(store.settings.limits.perHour) 条；安静时段内的通知会在结束后合并发送。关闭“锁屏显示摘要”时，通知只写“你的 Agent 有新消息”。")
        }
    }

    private var chatSection: some View {
        Section {
            Toggle("显示执行步骤", isOn: binding({ store.settings.showSteps }, { v in store.updateSettings(["showSteps": v]) }))
            Toggle("分析我的 Codex 会话", isOn: binding({ store.settings.sessionAnalysis }, { v in store.updateSettings(["sessionAnalysis": v]) }))
            Button("现在分析最近 7 天的会话") { store.analyzeSessions() }
                .disabled(!store.settings.sessionAnalysis)
        } header: {
            Text("对话")
        } footer: {
            Text("会话分析只读取这台 Mac 上你自己的 Codex 会话，结果只给你看。")
        }
    }

    private func row(_ title: String, _ value: String) -> some View {
        HStack {
            Text(title)
            Spacer()
            Text(value).foregroundStyle(.secondary).lineLimit(1)
        }
    }

    private func timeBinding(_ path: KeyPath<Settings, String>, key: String) -> Binding<Date> {
        Binding(
            get: {
                let parts = store.settings[keyPath: path].split(separator: ":").compactMap { Int($0) }
                var c = Calendar.current.dateComponents([.year, .month, .day], from: Date())
                c.hour = parts.first ?? 9
                c.minute = parts.count > 1 ? parts[1] : 0
                return Calendar.current.date(from: c) ?? Date()
            },
            set: { d in
                let c = Calendar.current.dateComponents([.hour, .minute], from: d)
                store.updateSettings([key: String(format: "%02d:%02d", c.hour ?? 0, c.minute ?? 0)])
            }
        )
    }
}

/// Mac only: folders the agent may write to.
struct WorkspaceRootsEditor: View {
    @EnvironmentObject var store: LoopMeStore
    @State private var newRoot = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("授权文件夹").font(.subheadline)
            ForEach(store.settings.workspaceRoots, id: \.self) { r in
                HStack {
                    Text(r).font(.caption.monospaced())
                    Spacer()
                    Button {
                        store.updateSettings(["workspaceRoots": store.settings.workspaceRoots.filter { $0 != r }])
                    } label: { Image(systemName: "minus.circle") }
                    .buttonStyle(.plain)
                }
            }
            HStack {
                TextField("~/code/项目", text: $newRoot)
                Button("添加") {
                    let r = newRoot.trimmingCharacters(in: .whitespaces)
                    guard !r.isEmpty else { return }
                    store.updateSettings(["workspaceRoots": store.settings.workspaceRoots + [r]])
                    newRoot = ""
                }
            }
        }
    }
}

public struct MemoryView: View {
    @EnvironmentObject var store: LoopMeStore
    @State private var adding = false
    @State private var newText = ""
    @State private var editing: MemoryEntry?
    @State private var editText = ""

    public init() {}

    private var active: [MemoryEntry] { store.memory.filter { $0.status != "deleted" } }
    private var deleted: [MemoryEntry] { store.memory.filter { $0.status == "deleted" } }

    public var body: some View {
        List {
            Section {
                if active.isEmpty {
                    Text("还没有记忆。Agent 会在聊天中记下关于你的稳定事实，你也可以自己添加。")
                        .font(.footnote).foregroundStyle(.secondary)
                }
                ForEach(active) { m in
                    HStack(alignment: .top, spacing: 8) {
                        Image(systemName: m.status == "locked" ? "lock.fill" : (m.author == "user" ? "person.fill" : "sparkles"))
                            .font(.caption).foregroundStyle(.secondary).frame(width: 16)
                        Text(m.text)
                    }
                    .contextMenu { menu(m) }
                    .swipeActions {
                        Button("删除", role: .destructive) { store.deleteMemory(m) }
                        Button(m.status == "locked" ? "解锁" : "锁定") { store.updateMemory(m, locked: m.status != "locked") }
                    }
                }
            } header: {
                Text("记忆")
            } footer: {
                Text("锁定的记忆 Agent 不能修改。删除的内容进入“不要再记”，之后不会被写回。")
            }
            if !deleted.isEmpty {
                Section("不要再记") {
                    ForEach(deleted) { m in
                        HStack {
                            Text(m.text).strikethrough().foregroundStyle(.secondary)
                            Spacer()
                            Button("恢复") { store.restoreMemory(m) }.font(.caption)
                        }
                    }
                }
            }
        }
        .navigationTitle("记忆")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { adding = true } label: { Image(systemName: "plus") }
            }
        }
        .alert("添加记忆", isPresented: $adding) {
            TextField("例如：我习惯用 pnpm", text: $newText)
            Button("添加") {
                if !newText.isEmpty { store.addMemory(newText) }
                newText = ""
            }
            Button("取消", role: .cancel) { newText = "" }
        }
        .sheet(item: $editing) { m in
            NavigationStack {
                TextEditor(text: $editText)
                    .padding()
                    .navigationTitle("编辑记忆")
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) { Button("取消") { editing = nil } }
                        ToolbarItem(placement: .confirmationAction) {
                            Button("保存") {
                                store.updateMemory(m, text: editText)
                                editing = nil
                            }
                        }
                    }
            }
            .frame(minWidth: 360, minHeight: 240)
        }
    }

    @ViewBuilder private func menu(_ m: MemoryEntry) -> some View {
        Button("编辑") {
            editText = m.text
            editing = m
        }
        Button(m.status == "locked" ? "解锁" : "锁定") { store.updateMemory(m, locked: m.status != "locked") }
        Button("删除", role: .destructive) { store.deleteMemory(m) }
    }
}

public struct ProfileView: View {
    @EnvironmentObject var store: LoopMeStore
    @State private var draft = Profile()
    @State private var loaded = false

    public init() {}

    public var body: some View {
        Form {
            Section {
                TextField("称呼", text: $draft.name)
                TextField("时区，例如 Asia/Shanghai", text: $draft.timezone)
                TextField("工作时间，例如 工作日 10:00–19:00", text: $draft.workHours)
            } footer: {
                Text("Profile 的优先级高于记忆。")
            }
            Section("关于我") {
                TextEditor(text: $draft.about).frame(minHeight: 120)
            }
            Section {
                Button("保存") {
                    store.updateProfile(["name": draft.name, "timezone": draft.timezone, "workHours": draft.workHours, "about": draft.about])
                }
                .disabled(draft == store.profile || !store.isConnected)
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Profile")
        .onAppear {
            if !loaded {
                draft = store.profile
                if draft.timezone.isEmpty { draft.timezone = TimeZone.current.identifier }
                loaded = true
            }
        }
    }
}
