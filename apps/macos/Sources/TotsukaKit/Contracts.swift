import Foundation

/// `totsuka menu --json` (ADR-0065's `MenuModel`).
public struct MenuModel: Decodable, Equatable, Sendable {
    public let availability: String
    public let attentionCount: Int
    public let attention: [MenuRow]
    public let working: [MenuRow]
    public let degraded: [String]
    public let error: String?

    public static func decode(_ data: Data) throws -> MenuModel {
        try snakeCaseDecoder.decode(MenuModel.self, from: data)
    }
}

/// One task row of [`MenuModel`].
public struct MenuRow: Decodable, Equatable, Hashable, Sendable, Identifiable {
    public let taskId: Int64
    public let state: String
    public let workflow: String
    public let title: String
    /// The repository the task resolved to; absent until it has one (and
    /// from a CLI older than the field).
    public let repo: String?
    /// When the task was ingested (RFC 3339), what the row's elapsed time
    /// counts from. Absent from a CLI older than the field.
    public let createdAt: String?
    /// What a task waits on when its state alone does not say: `approval`
    /// for a `running` task stopped at a permission prompt.
    public let waitingFor: String?

    public var id: Int64 { taskId }

    /// The row's second line: state, how long ago the task came in,
    /// repository and workflow — most telling first, since a narrow panel
    /// truncates the tail — leaving out whatever is not known.
    public func detail(now: Date = Date()) -> String {
        let shown = waitingFor == "approval" ? "awaiting approval" : state
        let elapsed = createdAt.flatMap(parseTimestamp).map { elapsedText(from: $0, to: now) }
        return [shown, elapsed, repo, workflow].compactMap { $0 }.joined(separator: " · ")
    }
}

/// An RFC 3339 timestamp as the CLI writes it, with or without fractional
/// seconds.
public func parseTimestamp(_ text: String) -> Date? {
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    if let date = formatter.date(from: text) { return date }
    formatter.formatOptions = [.withInternetDateTime]
    return formatter.date(from: text)
}

/// A short elapsed time: `45s`, `12m`, `3h 5m`, `2d 4h`.
public func elapsedText(from start: Date, to now: Date) -> String {
    let seconds = max(0, Int(now.timeIntervalSince(start)))
    switch seconds {
    case ..<60: return "\(seconds)s"
    case ..<3600: return "\(seconds / 60)m"
    case ..<86400: return "\(seconds / 3600)h \(seconds % 3600 / 60)m"
    default: return "\(seconds / 86400)d \(seconds % 86400 / 3600)h"
    }
}

/// One `notify` line of `run --events-jsonl` — the fields notifier plugins
/// receive as `NotifyParams`.
public struct NotifyEvent: Decodable, Equatable, Sendable {
    public let event: String
    public let taskId: String?
    public let workflow: String?
    public let title: String
    public let body: String?
}

/// One line of `run --events-jsonl` (`ai-docs/apis/run-events-jsonl.md`).
public enum RunEvent: Equatable, Sendable {
    case notify(NotifyEvent)
    case summary
    /// A line of a `type` this app does not know; skipped, per the contract.
    case other

    /// `nil` when the line is not a JSON object at all.
    public static func parse(_ line: String) -> RunEvent? {
        let data = Data(line.utf8)
        guard let value = JSONValue.parse(line), value.object != nil else { return nil }
        switch value["type"]?.string {
        case "notify":
            return (try? snakeCaseDecoder.decode(NotifyEvent.self, from: data)).map(RunEvent.notify)
                ?? .other
        case "summary": return .summary
        default: return .other
        }
    }
}

/// `totsuka config get`.
public struct ConfigDocument: Sendable {
    public let configPath: String
    public let exists: Bool
    public let config: JSONValue

    public static func decode(_ data: Data) throws -> ConfigDocument {
        let value = try JSONDecoder().decode(JSONValue.self, from: data)
        return ConfigDocument(
            configPath: value["config_path"]?.string ?? "",
            exists: value["exists"]?.bool ?? false,
            config: value["config"] ?? .object([:]))
    }
}

let snakeCaseDecoder: JSONDecoder = {
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    return decoder
}()
