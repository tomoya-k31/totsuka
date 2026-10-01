import Foundation

/// What a finished `totsuka` invocation left behind.
public struct CommandResult: Sendable {
    public let status: Int32
    public let stdout: Data
    public let stderr: Data

    public var stdoutText: String { String(decoding: stdout, as: UTF8.self) }
    public var stderrText: String { String(decoding: stderr, as: UTF8.self) }

    /// The message of the JSON error envelope the app-facing commands put on
    /// stderr (`{"error":{"message":…,"action":…}}`), else the raw stderr.
    public var errorMessage: String {
        let line = stderrText.split(separator: "\n").last.map(String.init) ?? ""
        if let envelope = JSONValue.parse(line), let error = envelope["error"] {
            let message = error["message"]?.string ?? ""
            if let action = error["action"]?.string { return "\(message) → \(action)" }
            return message
        }
        return stderrText.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// Runs the `totsuka` CLI. One binary, one environment: the app hands every
/// call the login shell's environment so `claude`, `git`, `gh` and the
/// agent IDE resolve the way they do in a terminal (ADR-0100's note to the
/// launcher).
public struct TotsukaCLI: Sendable {
    public let binary: URL
    public let environment: [String: String]

    public init(binary: URL, environment: [String: String]) {
        self.binary = binary
        self.environment = environment
    }

    /// Run `totsuka <arguments>` to completion, feeding `stdin` if given.
    public func run(_ arguments: [String], stdin: Data? = nil) async throws -> CommandResult {
        let binary = binary
        let environment = environment
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global().async {
                do {
                    continuation.resume(
                        returning: try Self.runBlocking(binary, arguments, environment, stdin))
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }

    private static func runBlocking(
        _ binary: URL, _ arguments: [String], _ environment: [String: String], _ stdin: Data?
    ) throws -> CommandResult {
        let process = Process()
        process.executableURL = binary
        process.arguments = arguments
        process.environment = environment
        let out = Pipe()
        let err = Pipe()
        process.standardOutput = out
        process.standardError = err
        let input = Pipe()
        process.standardInput = stdin == nil ? FileHandle.nullDevice : input
        try process.run()
        if let stdin {
            input.fileHandleForWriting.write(stdin)
            try? input.fileHandleForWriting.close()
        }
        // Both pipes are drained concurrently: a child that fills one while
        // the parent blocks on the other would never exit.
        var errData = Data()
        let group = DispatchGroup()
        group.enter()
        DispatchQueue.global().async {
            errData = err.fileHandleForReading.readDataToEndOfFile()
            group.leave()
        }
        let outData = out.fileHandleForReading.readDataToEndOfFile()
        group.wait()
        process.waitUntilExit()
        return CommandResult(status: process.terminationStatus, stdout: outData, stderr: errData)
    }
}

/// The environment a login shell would give a terminal (`$SHELL -lic env`), so
/// a GUI-launched `run` finds the same tools. Falls back to this process's own
/// environment when the shell cannot be run or takes longer than `timeout`
/// (an rc file waiting for input must not hang the app).
public func loginShellEnvironment(timeout: TimeInterval = 10) -> [String: String] {
    let fallback = ProcessInfo.processInfo.environment
    let shell = fallback["SHELL"] ?? "/bin/zsh"
    let process = Process()
    process.executableURL = URL(fileURLWithPath: shell)
    process.arguments = ["-lic", "env"]
    let out = Pipe()
    process.standardOutput = out
    process.standardError = FileHandle.nullDevice
    process.standardInput = FileHandle.nullDevice
    let exited = DispatchSemaphore(value: 0)
    process.terminationHandler = { _ in exited.signal() }
    guard (try? process.run()) != nil else { return fallback }
    var data = Data()
    let read = DispatchSemaphore(value: 0)
    DispatchQueue.global().async {
        data = out.fileHandleForReading.readDataToEndOfFile()
        read.signal()
    }
    guard exited.wait(timeout: .now() + timeout) == .success else {
        process.terminate()
        return fallback
    }
    read.wait()
    let parsed = parseEnv(String(decoding: data, as: UTF8.self))
    return parsed["PATH"] == nil ? fallback : parsed
}

/// `KEY=value` lines (`env` output) as a dictionary. A line without `=` (the
/// tail of a multi-line value) is skipped.
public func parseEnv(_ text: String) -> [String: String] {
    var env: [String: String] = [:]
    for line in text.split(separator: "\n") {
        guard let eq = line.firstIndex(of: "="), eq != line.startIndex else { continue }
        let key = String(line[..<eq])
        guard key.allSatisfy({ $0.isLetter || $0.isNumber || $0 == "_" }) else { continue }
        env[key] = String(line[line.index(after: eq)...])
    }
    return env
}

/// Where `totsuka` is: the configured path, else the first `totsuka` on the
/// login `PATH`, else Homebrew's usual locations.
public func locateTotsuka(override: String?, environment: [String: String]) -> URL? {
    let fm = FileManager.default
    if let override, !override.isEmpty {
        let url = URL(fileURLWithPath: (override as NSString).expandingTildeInPath)
        return fm.isExecutableFile(atPath: url.path) ? url : nil
    }
    let path = (environment["PATH"] ?? "").split(separator: ":").map(String.init)
    for dir in path + ["/opt/homebrew/bin", "/usr/local/bin"] {
        let candidate = URL(fileURLWithPath: dir).appendingPathComponent("totsuka")
        if fm.isExecutableFile(atPath: candidate.path) { return candidate }
    }
    return nil
}

/// `$XDG_STATE_HOME/totsuka`, resolved as the CLI resolves it.
public func stateDirectory(environment: [String: String]) -> URL {
    if let xdg = environment["XDG_STATE_HOME"], !xdg.isEmpty {
        return URL(fileURLWithPath: xdg).appendingPathComponent("totsuka")
    }
    let home = environment["HOME"] ?? NSHomeDirectory()
    return URL(fileURLWithPath: home).appendingPathComponent(".local/state/totsuka")
}
