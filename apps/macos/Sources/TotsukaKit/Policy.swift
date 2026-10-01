import Foundation

/// What to do when the supervised `run` exits (ADR-0109 §2; exit codes from
/// ADR-0095).
public enum ExitDecision: Equatable, Sendable {
    /// Stopped because it was asked to: stay stopped.
    case stopped
    /// A failure that may pass: start again after `delay` seconds.
    case restart(delay: TimeInterval)
    /// Restarting cannot help; a person has to fix something first.
    case fail(reason: ExitReason)
    /// Another `run` holds the lock: watch it instead of fighting it.
    case externalRun
}

public enum ExitReason: Equatable, Sendable {
    /// Exit 4: configuration or secrets.
    case configuration
    /// Exit 2: a usage error — this app's own bug.
    case usage
}

/// The exit-code policy. `consecutiveFailures` counts the restarts already
/// made without a healthy run in between; the delay doubles from 2 s up to
/// 5 minutes. `bySignal` is whether the process was killed rather than
/// exiting — then `status` is the signal number, and SIGINT's 2 must not read
/// as exit code 2.
public func exitDecision(
    status: Int32, bySignal: Bool, requestedStop: Bool, consecutiveFailures: Int
) -> ExitDecision {
    if requestedStop { return .stopped }
    switch bySignal ? -1 : status {
    // `run --watch` exits 0 only after a graceful stop: someone stopped it
    // (`kill`, logout). Starting it again would overrule them.
    case 0: return .stopped
    case 2: return .fail(reason: .usage)
    case 4: return .fail(reason: .configuration)
    case 5: return .externalRun
    default:
        let exponent = min(consecutiveFailures, 8)
        return .restart(delay: min(2 * pow(2, Double(exponent)), 300))
    }
}

/// `totsuka --version` → `(major, minor, patch)`.
public func parseVersion(_ text: String) -> (Int, Int, Int)? {
    guard let token = text.split(whereSeparator: \.isWhitespace).last else { return nil }
    let parts = token.split(separator: ".").compactMap { Int($0) }
    guard parts.count == 3 else { return nil }
    return (parts[0], parts[1], parts[2])
}

public enum VersionVerdict: Equatable, Sendable {
    case match
    /// Minor or patch differ: run, but say so.
    case warn
    /// Major differs: the CLI contract may have changed; refuse to start.
    case block
}

/// ADR-0109 §6: major mismatch blocks, minor/patch mismatch warns.
public func compareVersions(app: (Int, Int, Int), cli: (Int, Int, Int)) -> VersionVerdict {
    if app.0 != cli.0 { return .block }
    if app.1 != cli.1 || app.2 != cli.2 { return .warn }
    return .match
}

/// notifier-macos's filter (F-92), applied by the app since it is the
/// notifier under `--events-jsonl`: the workflow's toggle wins, then the
/// global one, and an event nobody mentions is delivered.
public func shouldNotify(_ event: NotifyEvent, macos: JSONValue?) -> Bool {
    let filter = macos?["filter"]
    if let workflow = event.workflow,
        let toggle = filter?["workflows"]?[workflow]?[event.event]?.bool
    {
        return toggle
    }
    return filter?["events"]?[event.event]?.bool ?? true
}

/// The `secret:<name>` name for a config field, from its JSON Pointer
/// segments: `["github", "token"]` → `github.token`. The CLI allows only
/// `[A-Za-z0-9_.-]` in a name, and tool or plugin names may hold anything, so
/// every other byte — and `_` and `.` themselves — is written as `_XX` (hex)
/// inside a segment. That keeps the mapping one-to-one: `a/b` and `a_b`, or
/// `["a.b"]` and `["a", "b"]`, can never share a Keychain entry.
public func secretName(for segments: [String]) -> String {
    segments.map { segment in
        segment.utf8.map { byte -> String in
            let c = Character(UnicodeScalar(byte))
            if byte < 0x80, c.isLetter || c.isNumber || c == "-" { return String(c) }
            return String(format: "_%02X", byte)
        }.joined()
    }.joined(separator: ".")
}
