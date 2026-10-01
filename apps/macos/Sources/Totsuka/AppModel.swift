import AppKit
import ServiceManagement
import TotsukaKit
import UserNotifications

/// The texts of the app's own UI, in the user's language — the same `{en, ja}`
/// choice the settings window makes for the schema's texts.
func L(_ en: String, _ ja: String) -> String { preferredLanguage == "ja" ? ja : en }

/// Everything the menu shows and does: the supervised `run`, its
/// notifications, the polled `menu --json` (ADR-0109 §2–§4).
@MainActor
final class AppModel: ObservableObject {
    enum RunState: Equatable {
        case stopped
        case starting
        case running
        case stopping
        case restarting(at: Date)
        case failed(String)
        /// Another `run` holds the lock (exit 5).
        case external
    }

    @Published private(set) var runState: RunState = .stopped
    @Published private(set) var menu: MenuModel?
    /// A problem to show above the menu (CLI missing, version mismatch, …).
    @Published private(set) var notice: String?
    /// The bundle this process was started from is gone (`brew upgrade` +
    /// cleanup): offer a restart into the new one.
    @Published private(set) var needsRestart = false
    @Published private(set) var logLines: [String] = []

    private(set) var cli: TotsukaCLI?
    private var process: RunProcess?
    private var requestedStop = false
    private var failures = 0
    private var startedAt = Date.distantPast
    private var killTimer: Timer?
    private var restartTimer: Timer?
    private var macosConfig: JSONValue?
    private var versionBlocked = false
    /// Set by `restartIntoUpdate`: open the new app once the child has exited,
    /// so the new instance does not find the old `run` still holding the lock.
    private var relaunchURL: URL?
    let secrets = SecretStore()
    private let defaults = UserDefaults.standard

    private static let logLimit = 500
    /// The longest a graceful stop is waited for: `run` may sit on a hung git
    /// that long (ADR-0092).
    private static let stopGrace: TimeInterval = 300

    init() {
        Task { await bootstrap() }
    }

    var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
    }

    // MARK: - startup

    func bootstrap() async {
        let pathOverride = defaults.string(forKey: "pathOverride") ?? ""
        let binaryOverride = defaults.string(forKey: "totsukaPath")
        var env = await Task.detached { loginShellEnvironment() }.value
        if !pathOverride.isEmpty { env["PATH"] = pathOverride }
        guard let binary = locateTotsuka(override: binaryOverride, environment: env) else {
            notice = L(
                "totsuka was not found → install it with Homebrew, or set its path in Settings",
                "totsuka が見つからない → Homebrew で入れるか、設定でパスを指定する")
            return
        }
        cli = TotsukaCLI(binary: binary, environment: env)
        await checkVersion()
        _ = try? await UNUserNotificationCenter.current().requestAuthorization(options: [
            .alert, .sound,
        ])
        startPolling()
        // After an update the login item still points at the old Cellar copy;
        // registering again moves it to this one.
        if SMAppService.mainApp.status == .enabled { try? SMAppService.mainApp.register() }
        if defaults.bool(forKey: "wasRunning") { await start() }
    }

    /// ADR-0109 §6: major mismatch blocks, minor/patch mismatch warns. A
    /// development build has no version and is not checked.
    private func checkVersion() async {
        guard let cli, let app = parseVersion(appVersion),
            let result = try? await cli.run(["--version"]),
            let installed = parseVersion(result.stdoutText)
        else { return }
        switch compareVersions(app: app, cli: installed) {
        case .match: break
        case .warn:
            notice = L(
                "totsuka \(installed.0).\(installed.1).\(installed.2) differs from the app (\(appVersion)) → brew upgrade totsuka",
                "totsuka \(installed.0).\(installed.1).\(installed.2) とアプリ（\(appVersion)）の版が違う → brew upgrade totsuka")
        case .block:
            versionBlocked = true
            notice = L(
                "totsuka \(installed.0).x does not match the app (\(appVersion)) → brew upgrade totsuka",
                "totsuka \(installed.0).x とアプリ（\(appVersion)）のメジャー版が違うので起動しない → brew upgrade totsuka")
        }
    }

    // MARK: - run

    func start() async {
        guard let cli, process == nil, !versionBlocked else { return }
        restartTimer?.invalidate()
        runState = .starting
        guard let secretMap = loadSecrets() else { return }
        // The start gate (ADR-0109 §2): nothing runs until the config passes.
        let check = try? await cli.run(
            ["config", "validate", "--secrets-stdin"], stdin: secretsLine(secretMap))
        // A stop pressed while the check ran wins (`stop` left `.starting`).
        guard runState == .starting else { return }
        guard let check, check.status == 0 else {
            let output = [check?.stdoutText, check?.stderrText].compactMap { $0 }.joined()
            fail(L("The configuration does not pass → open Settings", "設定が通らない → 設定を開く")
                + "\n" + output.trimmingCharacters(in: .whitespacesAndNewlines))
            return
        }
        if let config = try? await cli.run(["config", "get"]),
            let doc = try? ConfigDocument.decode(config.stdout)
        {
            macosConfig = doc.config["macos"]
        }
        guard runState == .starting else { return }
        let child = RunProcess(
            cli: cli,
            onEvent: { [weak self] event in Task { @MainActor in self?.handle(event) } },
            onLog: { [weak self] line in Task { @MainActor in self?.appendLog(line) } },
            onExit: { [weak self] exit in Task { @MainActor in self?.exited(exit) } })
        do {
            try child.start(secrets: secretMap)
        } catch {
            fail(error.localizedDescription)
            return
        }
        process = child
        requestedStop = false
        startedAt = Date()
        runState = .running
        defaults.set(true, forKey: "wasRunning")
        await refreshMenu()
    }

    func stop() {
        defaults.set(false, forKey: "wasRunning")
        restartTimer?.invalidate()
        guard let process else {
            runState = .stopped
            return
        }
        terminateGracefully(process)
    }

    /// SIGTERM, then SIGKILL if `run` has not exited within `stopGrace`.
    private func terminateGracefully(_ process: RunProcess) {
        requestedStop = true
        runState = .stopping
        process.terminate()
        killTimer = Timer.scheduledTimer(withTimeInterval: Self.stopGrace, repeats: false) {
            [weak self] _ in Task { @MainActor in self?.forceStop() }
        }
    }

    func forceStop() {
        requestedStop = true
        process?.kill()
    }

    private func exited(_ exit: RunProcess.Termination) {
        process = nil
        killTimer?.invalidate()
        if let relaunchURL {
            openAndQuit(relaunchURL)
            return
        }
        // A run that stayed up a minute was healthy: the next failure starts
        // the backoff over.
        if Date().timeIntervalSince(startedAt) > 60 { failures = 0 }
        switch exitDecision(
            status: exit.status, bySignal: exit.bySignal, requestedStop: requestedStop,
            consecutiveFailures: failures)
        {
        case .stopped:
            runState = .stopped
        case .restart(let delay):
            failures += 1
            let at = Date().addingTimeInterval(delay)
            runState = .restarting(at: at)
            restartTimer = Timer.scheduledTimer(withTimeInterval: delay, repeats: false) {
                [weak self] _ in Task { @MainActor in await self?.start() }
            }
        case .fail(.configuration):
            fail(L("Configuration or secrets were refused (exit 4) → open Settings",
                   "設定か機密情報が受け付けられなかった（exit 4）→ 設定を開く")
                + "\n" + recentLog())
        case .fail(.usage):
            fail(L("totsuka refused the app's arguments (exit 2) → update both",
                   "totsuka がアプリの引数を受け付けなかった（exit 2）→ 両方を更新する")
                + "\n" + recentLog())
        case .externalRun:
            runState = .external
        }
    }

    private func fail(_ message: String) {
        runState = .failed(message)
        defaults.set(false, forKey: "wasRunning")
    }

    private func recentLog() -> String {
        logLines.suffix(5).joined(separator: "\n")
    }

    private func appendLog(_ line: String) {
        logLines.append(line)
        if logLines.count > Self.logLimit { logLines.removeFirst(logLines.count - Self.logLimit) }
    }

    // MARK: - secrets

    /// The Keychain map. Before the first read by a new version, say that macOS
    /// is about to ask — an ad-hoc-signed update is a new app to the Keychain,
    /// so it asks once per update (measured, ADR-0109).
    func loadSecrets() -> [String: String]? {
        let seen = defaults.string(forKey: "keychainVersion")
        if let seen, seen != appVersion {
            let alert = NSAlert()
            alert.messageText = L("Keychain access", "キーチェーンへのアクセス")
            alert.informativeText = L(
                "Totsuka was updated, so macOS will ask once more for its saved secrets. Choose “Always Allow”.",
                "Totsuka が更新されたので、保存した機密情報について macOS がもう一度確認する。「常に許可」を選ぶ。")
            NSApp.activate()
            alert.runModal()
        }
        do {
            let map = try secrets.load()
            defaults.set(appVersion, forKey: "keychainVersion")
            return map
        } catch {
            fail(L("Keychain: ", "キーチェーン: ") + String(describing: error))
            return nil
        }
    }

    func secretsLine(_ map: [String: String]) -> Data {
        var data = (try? JSONEncoder().encode(map)) ?? Data("{}".utf8)
        data.append(UInt8(ascii: "\n"))
        return data
    }

    // MARK: - notifications

    private func handle(_ event: RunEvent) {
        guard case .notify(let notify) = event else { return }
        Task { await refreshMenu() }
        guard shouldNotify(notify, macos: macosConfig) else { return }
        let content = UNMutableNotificationContent()
        content.title = notify.title
        content.body = notify.body ?? eventLabel(notify.event)
        content.sound = .default
        if let id = notify.taskId { content.userInfo = ["task_id": id] }
        UNUserNotificationCenter.current().add(
            UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
    }

    private func eventLabel(_ event: String) -> String {
        switch event {
        case "waiting_input": return L("Waiting for your input", "入力を待っている")
        case "done": return L("Done", "完了")
        case "failed": return L("Failed", "失敗")
        case "pending": return L("Waiting for a repository choice", "リポジトリの選択を待っている")
        case "escalated": return L("Handed to you", "あなたに渡された")
        case "verification_pending": return L("Waiting for your verification", "検収を待っている")
        default: return event
        }
    }

    // MARK: - menu

    private func startPolling() {
        Task { await refreshMenu() }
        Timer.scheduledTimer(withTimeInterval: 10, repeats: true) { [weak self] _ in
            Task { @MainActor in
                await self?.refreshMenu()
                self?.checkBundle()
            }
        }
    }

    func refreshMenu() async {
        guard let cli, let result = try? await cli.run(["menu", "--json"]) else { return }
        menu = try? MenuModel.decode(result.stdout)
        // The outside `run` (exit 5) has let go of the lock: take over if this
        // app was meant to be running it.
        if runState == .external, menu?.availability == "down" {
            runState = .stopped
            if defaults.bool(forKey: "wasRunning") { await start() }
        }
    }

    /// The task actions go through the CLI, which already knows the socket and
    /// its token (ADR-0094 / ADR-0099).
    func focus(_ id: String) {
        Task { await act(["focus", id]) }
    }

    func retry(_ id: Int64) {
        Task { await act(["task", "retry", String(id)]) }
    }

    /// Run a task action and show its error, if any, above the menu.
    private func act(_ arguments: [String]) async {
        guard let cli else { return }
        do {
            let r = try await cli.run(arguments)
            notice = r.status == 0 ? nil : r.errorMessage
        } catch {
            notice = String(describing: error)
        }
        await refreshMenu()
    }

    func cancel(_ row: MenuRow) {
        let alert = NSAlert()
        alert.messageText = L("Cancel this task?", "このタスクを取り消すか？")
        alert.informativeText = row.title
        alert.addButton(withTitle: L("Cancel task", "取り消す"))
        alert.addButton(withTitle: L("Keep", "やめる"))
        NSApp.activate()
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        Task { await act(["task", "cancel", String(row.taskId)]) }
    }

    // MARK: - updates and login item

    private func checkBundle() {
        if !FileManager.default.fileExists(atPath: Bundle.main.bundlePath) { needsRestart = true }
    }

    /// Start the copy of the app next to the installed CLI (Homebrew puts both
    /// in the same prefix) and quit this one. The new instance re-registers
    /// the login item at its own path.
    func restartIntoUpdate() {
        guard let cli else { return }
        let app = cli.binary.resolvingSymlinksInPath()
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Totsuka.app")
        guard let process else {
            openAndQuit(app)
            return
        }
        relaunchURL = app
        terminateGracefully(process)
    }

    private func openAndQuit(_ app: URL) {
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.createsNewApplicationInstance = true
        NSWorkspace.shared.openApplication(at: app, configuration: configuration) { _, _ in
            Task { @MainActor in NSApp.terminate(nil) }
        }
    }

    var launchesAtLogin: Bool {
        get { SMAppService.mainApp.status == .enabled }
        set {
            if newValue {
                try? SMAppService.mainApp.register()
            } else {
                try? SMAppService.mainApp.unregister()
            }
            objectWillChange.send()
        }
    }

    func quit() {
        // `run` finishes its graceful stop on its own; `wasRunning` is kept so
        // the next launch starts it again.
        process?.terminate()
        NSApp.terminate(nil)
    }
}
