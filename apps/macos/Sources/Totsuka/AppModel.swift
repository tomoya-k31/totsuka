import AppKit
import ServiceManagement
import TotsukaKit
import UserNotifications

/// Everything the menu shows and does: the supervised `run`, its
/// notifications, the polled `menu --json` (ADR-0113 §2–§4).
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
    /// The last lines of `run`'s stderr, for the failure message. The full
    /// stream goes to `runLog`, which Logs follows in `$TERMINAL`.
    private var logLines: [String] = []
    private var runLog: RunLogFile?

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
    /// One Keychain entry per bundle ID, so a development build
    /// (`build-app.sh`, `….dev`) never reads or overwrites the real app's.
    let secrets = SecretStore(service: Bundle.main.bundleIdentifier ?? "io.github.tomoya-k31.totsuka")
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
        let login = await Task.detached { loginShellEnvironment() }.value
        var env = runEnvironment(login: login, own: ProcessInfo.processInfo.environment)
        if !pathOverride.isEmpty { env["PATH"] = pathOverride }
        guard let binary = locateExecutable(named: "totsuka", override: binaryOverride, environment: env) else {
            notice = "totsuka was not found → install it with Homebrew, or `defaults write \(Bundle.main.bundleIdentifier ?? "") totsukaPath /path/to/totsuka`"
            return
        }
        cli = TotsukaCLI(binary: binary, environment: env)
        runLog = RunLogFile(stateDirectory: stateDirectory(environment: env))
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

    /// ADR-0113 §6: major mismatch blocks, minor/patch mismatch warns. A
    /// development build has no version and is not checked.
    private func checkVersion() async {
        guard let cli, let app = parseVersion(appVersion),
            let result = try? await cli.run(["--version"]),
            let installed = parseVersion(result.stdoutText)
        else { return }
        switch compareVersions(app: app, cli: installed) {
        case .match: break
        case .warn:
            notice = "totsuka \(installed.0).\(installed.1).\(installed.2) differs from the app (\(appVersion)) → brew upgrade totsuka"
        case .block:
            versionBlocked = true
            notice = "totsuka \(installed.0).x does not match the app (\(appVersion)) → brew upgrade totsuka"
        }
    }

    // MARK: - run

    func start() async {
        guard let cli, process == nil, !versionBlocked else { return }
        restartTimer?.invalidate()
        runState = .starting
        let config = (try? await cli.run(["config", "get"])).flatMap {
            try? ConfigDocument.decode($0.stdout)
        }
        // A stop pressed while the file was read: ask for nothing.
        guard runState == .starting else { return }
        let document = config?.config ?? .object([:])
        // `secret:` values come from the Keychain map (asked for here when
        // missing — there is no settings window to enter them in). A config
        // without any is left to `run` to resolve, `op://` and `cmd:`
        // included (ADR-0113 §5).
        var secretMap: [String: String]?
        if usesSuppliedSecrets(document) {
            guard let stored = loadSecrets() else { return }
            let account = ghAccount(in: document)
            let githubSecret = githubTokenSecretName(in: document)
            guard
                let asked = askForMissingSecrets(
                    secretNames(in: document), in: stored,
                    github: githubSecret, gh: account)
            else {
                runState = .stopped
                return
            }
            secretMap = asked
            // ADR-0114: taken from `gh` at every start, never stored, so a
            // token `gh` has since replaced or revoked is never handed on.
            if defaults.bool(forKey: Self.githubTokenFromGh), let name = githubSecret {
                guard let token = await tokenFromGh(account) else { return }
                guard runState == .starting else { return }
                secretMap?[name] = token
            }
        }
        // The start gate (ADR-0113 §2): nothing runs until the config passes.
        let check = try? await cli.run(
            ["config", "validate"] + (secretMap == nil ? [] : ["--secrets-stdin"]),
            stdin: secretMap.map(secretsLine))
        // A stop pressed while the check ran wins (`stop` left `.starting`).
        guard runState == .starting else { return }
        guard let check, check.status == 0 else {
            let output = [check?.stdoutText, check?.stderrText].compactMap { $0 }.joined()
            fail("The configuration does not pass → fix config.toml (Settings… opens it)"
                + "\n" + output.trimmingCharacters(in: .whitespacesAndNewlines))
            return
        }
        macosConfig = config?.config["macos"]
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
        runLog?.begin()
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
            fail("Configuration or secrets were refused (exit 4) → fix config.toml (Settings… opens it)"
                + "\n" + recentLog())
        case .fail(.usage):
            fail("totsuka refused the app's arguments (exit 2) → update both"
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
        runLog?.append(line)
        logLines.append(line)
        if logLines.count > Self.logLimit { logLines.removeFirst(logLines.count - Self.logLimit) }
    }

    // MARK: - secrets

    /// The Keychain map. Before the first read by a new version, say that macOS
    /// is about to ask — an ad-hoc-signed update is a new app to the Keychain,
    /// so it asks once per update (measured, ADR-0113).
    func loadSecrets() -> [String: String]? {
        let seen = defaults.string(forKey: "keychainVersion")
        if let seen, seen != appVersion {
            let alert = NSAlert()
            alert.messageText = "Keychain access"
            alert.informativeText = "Totsuka was updated, so macOS will ask once more for its saved secrets. Choose “Always Allow”."
            NSApp.activate()
            alert.runModal()
        }
        do {
            let map = try secrets.load()
            defaults.set(appVersion, forKey: "keychainVersion")
            return map
        } catch {
            fail("Keychain: " + String(describing: error))
            return nil
        }
    }

    /// UserDefaults: `[github].token`'s secret comes from `gh auth token`.
    private static let githubTokenFromGh = "githubTokenFromGh"

    /// Ask for each name the map lacks, save the map, and return it — or nil
    /// when the person cancels (or the Keychain refuses), and nothing starts.
    /// `github` is the name `[github].token` refers to: with a `gh` account
    /// its question also offers `gh auth token`, and once chosen it is not
    /// asked for again.
    private func askForMissingSecrets(
        _ names: [String], in stored: [String: String], github: String?,
        gh: (host: String, login: String)?
    ) -> [String: String]? {
        var map = stored
        let fromGh = defaults.bool(forKey: Self.githubTokenFromGh)
        let missing = names.filter { map[$0] == nil && !(fromGh && $0 == github) }
        guard !missing.isEmpty else { return map }
        for name in missing {
            let alert = NSAlert()
            alert.messageText = "Secret “\(name)”"
            alert.informativeText = "config.toml refers to secret:\(name). Its value is kept in the Keychain and handed to totsuka run on start."
            let field = NSSecureTextField(frame: NSRect(x: 0, y: 0, width: 280, height: 24))
            alert.accessoryView = field
            alert.addButton(withTitle: "Save")
            alert.addButton(withTitle: "Cancel")
            let offerGh = gh != nil && name == github
            if let gh, offerGh {
                alert.addButton(withTitle: "Use gh auth token")
                // `gh auth refresh` only touches the active account.
                alert.informativeText += "\n\nOr take \(gh.login)'s token from gh at every start (nothing is stored). The board needs the project scope: gh auth switch --hostname \(gh.host) --user \(gh.login), then gh auth refresh --hostname \(gh.host) -s project"
            }
            alert.window.initialFirstResponder = field
            NSApp.activate()
            let answer = alert.runModal()
            if offerGh, answer == .alertThirdButtonReturn {
                defaults.set(true, forKey: Self.githubTokenFromGh)
                continue
            }
            guard answer == .alertFirstButtonReturn, !field.stringValue.isEmpty else {
                notice = "Not started: secret:\(name) has no value"
                return nil
            }
            map[name] = field.stringValue
        }
        do {
            try secrets.save(map)
        } catch {
            fail("Keychain: " + String(describing: error))
            return nil
        }
        return map
    }

    /// Drop every saved secret; the next start asks for them again.
    func forgetSecrets() {
        let alert = NSAlert()
        alert.messageText = "Forget all saved secrets?"
        alert.informativeText = "They are asked for again at the next start."
        alert.addButton(withTitle: "Forget")
        alert.addButton(withTitle: "Keep")
        NSApp.activate()
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        do {
            try secrets.save([:])
            defaults.removeObject(forKey: Self.githubTokenFromGh)
            notice = nil
        } catch {
            notice = "Keychain: " + String(describing: error)
        }
    }

    /// `gh auth token` for `[github]`'s host and login, or nil — with the
    /// reason shown and nothing started. The token itself is never shown.
    private func tokenFromGh(_ account: (host: String, login: String)?) async -> String? {
        guard let cli, let account else {
            fail("Cannot tell which GitHub host or account to ask gh for → set [github].api_url and github_login as plain values (Settings… opens config.toml), or Forget saved secrets… and enter the token")
            return nil
        }
        let host = account.host
        let arguments = ["auth", "token", "--hostname", host, "--user", account.login]
        guard let gh = locateExecutable(named: "gh", environment: cli.environment) else {
            fail("gh was not found → install it (brew install gh) and run gh auth login --hostname \(host)")
            return nil
        }
        let result = try? await TotsukaCLI(binary: gh, environment: cli.environment).run(arguments)
        // A stop pressed while gh ran wins over its failure.
        guard runState == .starting else { return nil }
        let token = result?.stdoutText.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard let result, result.status == 0, !token.isEmpty else {
            let reason = result?.stderrText.split(separator: "\n").first.map(String.init) ?? ""
            fail("gh auth token failed: \(reason)\n→ not logged in: gh auth login --hostname \(host); unknown flag --user: gh 2.40 or later is needed (brew upgrade gh)")
            return nil
        }
        return token
    }

    func secretsLine(_ map: [String: String]) -> Data {
        var data = (try? JSONEncoder().encode(map)) ?? Data("{}".utf8)
        data.append(UInt8(ascii: "\n"))
        return data
    }

    // MARK: - config file and logs

    /// Settings…: config.toml in `$EDITOR` inside `$TERMINAL`, else in the
    /// default text editor. Changes take effect at the next start.
    func openSettings() {
        Task {
            guard let cli, let result = try? await cli.run(["config", "get"]),
                let doc = try? ConfigDocument.decode(result.stdout)
            else {
                notice = "Could not find config.toml (totsuka config get failed)"
                return
            }
            guard doc.exists else {
                notice = "There is no config.toml yet at \(doc.configPath) → run `totsuka init`"
                return
            }
            if let line = editorCommand(environment: cli.environment, path: doc.configPath) {
                launch(line, what: "$TERMINAL -e $EDITOR")
            } else {
                launch("exec open -t \(shellQuote(doc.configPath))", what: "open -t")
            }
        }
    }

    /// Logs: `run`'s stderr followed with `tail -F` in `$TERMINAL`, else in
    /// Console.
    func openLogs() {
        guard let cli, let runLog else { return }
        let path = runLog.url.path
        if let line = tailCommand(environment: cli.environment, path: path) {
            launch(line, what: "$TERMINAL")
        } else {
            if !FileManager.default.fileExists(atPath: path) { runLog.append("") }
            launch("exec open -a Console \(shellQuote(path))", what: "open -a Console")
        }
    }

    /// Run a `/bin/sh -c` line with the login shell's environment, without
    /// waiting; a command that could not be found is reported in the menu.
    private func launch(_ line: String, what: String) {
        guard let cli else { return }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/sh")
        process.arguments = ["-c", line]
        process.environment = cli.environment
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        process.terminationHandler = { [weak self] p in
            // 126 / 127: the shell could not run or find the command.
            guard p.terminationStatus == 126 || p.terminationStatus == 127 else { return }
            Task { @MainActor in self?.notice = "Could not run \(what) → check it in your login shell" }
        }
        do {
            try process.run()
        } catch {
            notice = "Could not run \(what): \(error.localizedDescription)"
        }
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
        case "waiting_input": return "Waiting for your input"
        case "done": return "Done"
        case "failed": return "Failed"
        case "pending": return "Waiting for a repository choice"
        case "escalated": return "Handed to you"
        case "verification_pending": return "Waiting for your verification"
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
        alert.messageText = "Cancel this task?"
        alert.informativeText = row.title
        alert.addButton(withTitle: "Cancel task")
        alert.addButton(withTitle: "Keep")
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
        NSApp.terminate(nil)
    }

    /// The app is exiting by any route — Quit, logout, `osascript … quit`.
    /// Hand `run` its SIGTERM here rather than in `quit`, or every other route
    /// leaves it running unsupervised and the next launch finds the lock
    /// taken. `run` finishes its graceful stop on its own; `wasRunning` is
    /// kept so the next launch starts it again.
    func willTerminate() {
        process?.terminate()
    }
}
