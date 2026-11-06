import LoopMeKit
import SwiftUI

struct RootView: View {
    @EnvironmentObject var store: LoopMeStore
    @Environment(\.scenePhase) private var scenePhase
    @State private var incomingLink: String?
    @State private var showSettings = false

    var body: some View {
        Group {
            if store.credentials == nil {
                ConnectView(incomingLink: $incomingLink)
            } else {
                NavigationStack {
                    ChatView()
                        .navigationBarTitleDisplayMode(.inline)
                        .toolbar {
                            ToolbarItem(placement: .principal) { titleView }
                            ToolbarItem(placement: .topBarTrailing) {
                                Button { showSettings = true } label: { Image(systemName: "gearshape") }
                            }
                        }
                        .toolbarBackground(Theme.background, for: .navigationBar)
                        .navigationDestination(isPresented: $showSettings) {
                            SettingsForm(onUnpair: {
                                showSettings = false
                                Task { await store.unpair() }
                            })
                            .navigationTitle("设置")
                        }
                }
            }
        }
        .background(Theme.background.ignoresSafeArea())
        .onChange(of: scenePhase) { _, phase in
            // Opening the app counts as showing up: the first time today starts the brief.
            if phase == .active { store.appOpened() }
        }
        .onOpenURL { url in
            if store.credentials == nil { incomingLink = url.absoluteString }
        }
        .alert("出错了", isPresented: Binding(get: { store.lastError != nil }, set: { if !$0 { store.lastError = nil } })) {
            Button("好") { store.lastError = nil }
        } message: {
            Text(store.lastError ?? "")
        }
    }

    private var titleView: some View {
        VStack(spacing: 1) {
            Text(macName).font(.headline).foregroundStyle(Theme.text)
            HStack(spacing: 4) {
                Circle().fill(store.isConnected ? Theme.green : Theme.secondary).frame(width: 6, height: 6)
                Text(subtitle).font(.caption2).foregroundStyle(Theme.secondary)
            }
        }
    }

    private var macName: String {
        store.status.macName.isEmpty ? (store.credentials?.macName ?? "Mac") : store.status.macName
    }

    private var subtitle: String {
        guard store.isConnected else { return store.connection == .connecting ? "连接中" : "不可达" }
        switch store.status.agent {
        case "working": return "工作中"
        case "paused": return "已暂停"
        default: return "在线"
        }
    }
}
