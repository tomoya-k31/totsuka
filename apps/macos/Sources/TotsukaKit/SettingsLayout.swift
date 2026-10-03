import Foundation

/// One page of the settings window: the config keys it edits.
public struct SettingsPage: Equatable, Sendable, Identifiable {
    public let id: String
    public let keys: [String]
    /// Whether the page also holds the app's own settings (binary, PATH, …).
    public let app: Bool

    public init(id: String, keys: [String], app: Bool = false) {
        self.id = id
        self.keys = keys
        self.app = app
    }
}

/// The sidebar: a fixed order a person can learn, rather than whatever order
/// the categories sort into. `settings` are the core's tables; `plugins` are
/// the roster and each plugin's own table, in the order sources → agents →
/// notifier, with any plugin not known here last.
public struct SettingsLayout: Equatable, Sendable {
    public let general: SettingsPage
    public let settings: [SettingsPage]
    public let plugins: [SettingsPage]

    /// The core keys each page edits. `general` also carries the app's own
    /// settings.
    static let generalKeys = ["version", "max_concurrency", "worktree"]
    static let settingsKeys: [(String, [String])] = [
        ("repositories", ["repositories"]),
        ("projects", ["projects"]),
        ("workflows", ["workflows"]),
        ("tools", ["default_tool", "tools"]),
        ("llm", ["llm"]),
        ("log", ["log"]),
        ("hooks", ["hooks"]),
    ]
    /// Known plugins, grouped: task sources (GitHub / Notion, Slack /
    /// Discord), agents (herdr / orca), the notifier.
    static let pluginOrder = ["github", "notion", "slack", "discord", "herdr", "orca", "macos"]

    public init(schema: JSONValue) {
        let present = Set(schema["properties"]?.object?.keys.map { $0 } ?? [])
        general = SettingsPage(
            id: "general", keys: Self.generalKeys.filter(present.contains), app: true)
        settings = Self.settingsKeys.compactMap { id, keys in
            let shown = keys.filter(present.contains)
            return shown.isEmpty ? nil : SettingsPage(id: id, keys: shown)
        }
        let core = Set(Self.generalKeys + Self.settingsKeys.flatMap(\.1) + ["plugins"])
        let pluginKeys = present.subtracting(core)
        let ordered =
            Self.pluginOrder.filter(pluginKeys.contains)
            + pluginKeys.subtracting(Self.pluginOrder).sorted()
        // The roster (`[plugins.<name>]`) is not a page of its own: each
        // plugin's page shows its entry, and the sidebar toggles `enabled`.
        plugins = ordered.map { SettingsPage(id: $0, keys: [$0]) }
    }
}

/// What a reference field picks from — a value that must name something
/// configured elsewhere, so the form offers a list instead of a text box.
public enum Reference: Equatable, Sendable {
    /// AI tools: the built-in ones and every `[tools.<name>]`.
    case tool
    /// `[plugins.<name>]` of `kind = "agent_ide"`.
    case agent
    /// `[plugins.<name>]` of `kind = "task_source"`.
    case source
    /// `[[projects]].name`.
    case project
    /// `[[repositories]].name`.
    case repository
}

/// The AI tools `totsuka` knows without a `[tools.<name>]` entry.
public let builtinTools = ["claude", "codex", "opencode"]

/// Which reference a field is, by its path (array indices as `*`), and
/// whether it holds a list of them.
public func reference(for segments: [String]) -> (Reference, multiple: Bool)? {
    let path = segments.map { Int($0) != nil ? "*" : $0 }.joined(separator: "/")
    switch path {
    case "default_tool", "repositories/*/tool", "workflows/*/tool": return (.tool, false)
    case "workflows/*/agent": return (.agent, false)
    case "workflows/*/projects": return (.project, true)
    case "repositories/*/project": return (.project, false)
    case "projects/*/source": return (.source, false)
    case "slack/fallback_repo", "workflows/*/trigger/repo": return (.repository, false)
    case "slack/channel_groups/*/repos": return (.repository, true)
    default: return nil
    }
}

/// The names a reference can take, from the loaded config.
public func referenceOptions(_ reference: Reference, config: JSONValue) -> [String] {
    func names(_ array: String) -> [String] {
        (config[array]?.array ?? []).compactMap { $0["name"]?.string }
    }
    func plugins(of kind: String) -> [String] {
        (config["plugins"]?.object ?? [:])
            .filter { $0.value["kind"]?.string == kind }
            .map(\.key).sorted()
    }
    switch reference {
    case .tool:
        let configured = (config["tools"]?.object ?? [:]).keys.sorted()
        return builtinTools + configured.filter { !builtinTools.contains($0) }
    case .agent: return plugins(of: "agent_ide")
    case .source: return plugins(of: "task_source")
    case .project: return names("projects")
    case .repository: return names("repositories")
    }
}
