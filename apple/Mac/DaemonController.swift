import Foundation
import LoopMeKit

/// Starts the bundled `loopme-daemon` and connects the store to it.
///
/// If a daemon is already running for the same data directory (for example
/// one started from a terminal with `cargo run`), it is reused.
@MainActor
final class DaemonController: ObservableObject {
    @Published private(set) var state = "启动中"
    @Published private(set) var external = false

    let dataDirectory: URL = {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("LoopMe", isDirectory: true)
    }()

    private var process: Process?
    private var stopping = false
    private var restarts: [Date] = []
    private weak var store: LoopMeStore?

    var logURL: URL { dataDirectory.appendingPathComponent("daemon.log") }

    func start(store: LoopMeStore) {
        self.store = store
        try? FileManager.default.createDirectory(at: dataDirectory, withIntermediateDirectories: true)
        if let pid = runningDaemonPid() {
            external = true
            state = "已在运行（pid \(pid)）"
            connectWhenReady()
            return
        }
        launch()
    }

    func stop() {
        stopping = true
        if let p = process, p.isRunning {
            p.terminate()
        }
        process = nil
    }

    func restart() {
        stop()
        stopping = false
        external = false
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in self?.launch() }
    }

    private func daemonBinary() -> URL? {
        if let env = ProcessInfo.processInfo.environment["LOOPME_DAEMON"] {
            return URL(fileURLWithPath: env)
        }
        if let url = Bundle.main.url(forAuxiliaryExecutable: "loopme-daemon") {
            return url
        }
        return Bundle.main.url(forResource: "loopme-daemon", withExtension: nil)
    }

    /// The pid written by a live daemon for this data directory.
    private func runningDaemonPid() -> Int32? {
        let pidFile = dataDirectory.appendingPathComponent("daemon.pid")
        guard let s = try? String(contentsOf: pidFile, encoding: .utf8),
              let pid = Int32(s.trimmingCharacters(in: .whitespacesAndNewlines)), pid > 0 else { return nil }
        guard kill(pid, 0) == 0 else { return nil }
        // Guard against a stale pid file pointing at an unrelated process.
        let ps = Process()
        ps.executableURL = URL(fileURLWithPath: "/bin/ps")
        ps.arguments = ["-p", String(pid), "-o", "comm="]
        let pipe = Pipe()
        ps.standardOutput = pipe
        ps.standardError = FileHandle.nullDevice
        guard (try? ps.run()) != nil else { return nil }
        ps.waitUntilExit()
        let name = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        return name.contains("loopme-daemon") ? pid : nil
    }

    private func launch() {
        guard let bin = daemonBinary() else {
            state = "找不到 loopme-daemon（请检查构建脚本）"
            return
        }
        let p = Process()
        p.executableURL = bin
        p.arguments = ["--data-dir", dataDirectory.path]
        var env = ProcessInfo.processInfo.environment
        env["RUST_LOG"] = env["RUST_LOG"] ?? "info"
        p.environment = env
        if !FileManager.default.fileExists(atPath: logURL.path) {
            FileManager.default.createFile(atPath: logURL.path, contents: nil)
        }
        if let log = try? FileHandle(forWritingTo: logURL) {
            log.seekToEndOfFile()
            p.standardOutput = log
            p.standardError = log
        }
        p.terminationHandler = { [weak self] proc in
            let code = proc.terminationStatus
            Task { @MainActor in self?.exited(code) }
        }
        do {
            try p.run()
            process = p
            state = "运行中"
            connectWhenReady()
        } catch {
            state = "启动失败：\(error.localizedDescription)"
        }
    }

    private func exited(_ code: Int32) {
        process = nil
        guard !stopping else { return }
        state = "已退出（\(code)），正在重启"
        let now = Date()
        restarts = restarts.filter { now.timeIntervalSince($0) < 60 } + [now]
        if restarts.count > 5 {
            state = "反复退出，已停止重启。日志：\(logURL.path)"
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
            guard let self, !self.stopping else { return }
            if self.runningDaemonPid() != nil {
                self.external = true
                self.state = "已在运行"
                self.connectWhenReady()
            } else {
                self.launch()
            }
        }
    }

    /// Waits for `local-client.json`, then connects.
    private func connectWhenReady(attempt: Int = 0) {
        if let creds = MacCredentials.local(dataDirectory: dataDirectory) {
            if store?.credentials != creds {
                store?.start(with: creds)
            } else {
                store?.conn.reconnectNow()
            }
            return
        }
        guard attempt < 50 else {
            state = "daemon 没有就绪，日志：\(logURL.path)"
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { [weak self] in
            self?.connectWhenReady(attempt: attempt + 1)
        }
    }
}
