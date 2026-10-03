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

    public var id: Int64 { taskId }
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
