import Foundation
import Testing

@testable import TotsukaKit

@Suite struct ExitPolicyTests {
    @Test func restartableFailuresBackOffUpToFiveMinutes() {
        #expect(
            exitDecision(status: 1, bySignal: false, requestedStop: false, consecutiveFailures: 0)
                == .restart(delay: 2))
        #expect(
            exitDecision(status: 1, bySignal: false, requestedStop: false, consecutiveFailures: 3)
                == .restart(delay: 16))
        #expect(
            exitDecision(status: 1, bySignal: false, requestedStop: false, consecutiveFailures: 20)
                == .restart(delay: 300))
    }

    @Test func configurationUsageAndLockAreNotRestarted() {
        #expect(
            exitDecision(status: 4, bySignal: false, requestedStop: false, consecutiveFailures: 0)
                == .fail(reason: .configuration))
        #expect(
            exitDecision(status: 2, bySignal: false, requestedStop: false, consecutiveFailures: 0)
                == .fail(reason: .usage))
        #expect(
            exitDecision(status: 5, bySignal: false, requestedStop: false, consecutiveFailures: 0)
                == .externalRun)
    }

    /// SIGINT is signal 2: a killed process must not read as exit code 2.
    @Test func aSignalIsRestartedNotReadAsAnExitCode() {
        #expect(
            exitDecision(status: 2, bySignal: true, requestedStop: false, consecutiveFailures: 0)
                == .restart(delay: 2))
    }

    @Test func aStopThatWasAskedForOrAGracefulExitStaysStopped() {
        #expect(
            exitDecision(status: 1, bySignal: false, requestedStop: true, consecutiveFailures: 0)
                == .stopped)
        #expect(
            exitDecision(status: 0, bySignal: false, requestedStop: false, consecutiveFailures: 0)
                == .stopped)
    }
}

@Suite struct VersionTests {
    @Test func parsesTheCLIVersionLine() throws {
        let v = try #require(parseVersion("totsuka 0.10.3\n"))
        #expect(v == (0, 10, 3))
        #expect(parseVersion("totsuka dev") == nil)
    }

    @Test func majorBlocksMinorWarns() {
        #expect(compareVersions(app: (1, 0, 0), cli: (0, 10, 3)) == .block)
        #expect(compareVersions(app: (0, 11, 0), cli: (0, 10, 3)) == .warn)
        #expect(compareVersions(app: (0, 10, 3), cli: (0, 10, 3)) == .match)
    }
}

@Suite struct EventTests {
    @Test func parsesNotifyAndSummaryLines() {
        let line = #"{"type":"notify","event":"waiting_input","task_id":"7","workflow":"impl","title":"Fix"}"#
        guard case .notify(let event) = RunEvent.parse(line) else {
            Issue.record("not a notify event")
            return
        }
        #expect(event.taskId == "7")
        #expect(event.workflow == "impl")
        #expect(RunEvent.parse(#"{"type":"summary","stats":{}}"#) == .summary)
        #expect(RunEvent.parse(#"{"type":"later"}"#) == .other)
        #expect(RunEvent.parse("plain text") == nil)
    }

    @Test func decodesTheMenuModel() throws {
        let json = #"""
            {"availability":"ok","attention_count":1,
             "attention":[{"task_id":3,"state":"waiting_input","workflow":"w","title":"T"}],
             "working":[],"degraded":[],"error":null}
            """#
        let menu = try MenuModel.decode(Data(json.utf8))
        #expect(menu.attention.first?.taskId == 3)
        #expect(menu.attentionCount == 1)
    }

    /// The workflow's toggle wins over the global one; unmentioned is on.
    @Test func appliesTheMacosFilter() throws {
        let macos = try #require(
            JSONValue.parse(
                #"{"filter":{"events":{"done":false},"workflows":{"loud":{"done":true}}}}"#))
        func event(_ name: String, _ workflow: String?) -> NotifyEvent {
            NotifyEvent(event: name, taskId: "1", workflow: workflow, title: "t", body: nil)
        }
        #expect(!shouldNotify(event("done", "quiet"), macos: macos))
        #expect(shouldNotify(event("done", "loud"), macos: macos))
        #expect(shouldNotify(event("failed", "quiet"), macos: macos))
        #expect(shouldNotify(event("done", nil), macos: nil))
    }
}

@Suite struct SchemaTests {
    @Test func classifiesFields() throws {
        let schema = try #require(
            JSONValue.parse(
                #"""
                {"type":"object","required":["name"],"properties":{
                  "name":{"type":"string","x-title":"Name"},
                  "level":{"type":["string","null"]},
                  "mode":{"anyOf":[{"oneOf":[{"const":"plan"},{"const":"implement"}]},{"type":"null"}],
                          "x-title":"Mode"},
                  "token":{"type":"string","x-secret":true},
                  "tags":{"type":"array","items":{"type":"string"}},
                  "repos":{"type":"array","items":{"type":"object","properties":{"a":{"type":"string"}}}},
                  "tools":{"type":"object","additionalProperties":{"type":"object","properties":{"k":{"type":"string"}}}},
                  "trigger":{"type":"object"}
                }}
                """#))
        guard case .object(let props) = fieldKind(schema) else {
            Issue.record("not an object")
            return
        }
        #expect(props.first?.key == "name", "required keys come first")
        let kinds = Dictionary(uniqueKeysWithValues: props.map { ($0.key, fieldKind($0.schema)) })
        #expect(kinds["level"] == .string)
        #expect(kinds["mode"] == .choice(["plan", "implement"]))
        #expect(kinds["token"] == .secret)
        #expect(kinds["tags"] == .stringList)
        #expect(kinds["trigger"] == .raw(reason: nil))
        if case .list = kinds["repos"] {} else { Issue.record("repos is not a list") }
        if case .map = kinds["tools"] {} else { Issue.record("tools is not a map") }
        let mode = props.first { $0.key == "mode" }!.schema
        #expect(annotation(nonNull(mode), "x-title") == "Mode")
    }

    @Test func newValuesFillOnlyRequiredKeys() throws {
        let schema = try #require(
            JSONValue.parse(
                #"{"type":"object","required":["name","projects"],"properties":{"name":{"type":"string"},"projects":{"type":"array","items":{"type":"string"}},"tool":{"type":"string"}}}"#
            ))
        #expect(newValue(for: schema) == .object(["name": .string(""), "projects": .array([])]))
    }

    @Test func secretNamesAndPointers() {
        #expect(secretName(for: ["github", "token"]) == "github.token")
        #expect(secretName(for: ["slack", "bot_token"]) == "slack.bot_5Ftoken")
        // One-to-one (Copilot on #849): neither `/` vs `_` nor a dot inside a
        // segment vs a segment boundary may collide.
        #expect(secretName(for: ["t", "a/b"]) != secretName(for: ["t", "a_b"]))
        #expect(secretName(for: ["a.b"]) != secretName(for: ["a", "b"]))
        let allowed = Set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.-")
        #expect(secretName(for: ["tools", "日本 語/x"]).allSatisfy { allowed.contains($0) })
        #expect(JSONPointer.join(["tools", "my.tool", "a/b"]) == "/tools/my.tool/a~1b")
    }

    @Test func integersSurviveARoundTrip() throws {
        let value = try #require(JSONValue.parse(#"{"n":4,"r":0.8}"#))
        #expect(value.jsonText == #"{"n":4,"r":0.8}"#)
    }

    /// `open --env` reaches `totsuka`: own XDG_/TOTSUKA_ variables win, other
    /// own variables do not leak in.
    @Test func ownXdgVariablesWinOverTheLoginShell() {
        let env = runEnvironment(
            login: ["PATH": "/login", "XDG_CONFIG_HOME": "/real"],
            own: ["XDG_CONFIG_HOME": "/isolated", "TOTSUKA_LOG_LEVEL": "debug", "PATH": "/gui"])
        #expect(env["XDG_CONFIG_HOME"] == "/isolated")
        #expect(env["TOTSUKA_LOG_LEVEL"] == "debug")
        #expect(env["PATH"] == "/login")
    }

    @Test func parsesEnvOutput() {
        let env = parseEnv("PATH=/a:/b\nHOME=/h\nMULTI=line1\nline2\n=bad\n")
        #expect(env["PATH"] == "/a:/b")
        #expect(env["MULTI"] == "line1")
        #expect(env.count == 3)
    }
}

@Suite struct SettingsLayoutTests {
    @Test func pagesHaveAFixedOrderAndPluginsAreGrouped() throws {
        let keys = ["version", "max_concurrency", "worktree", "repositories", "projects", "workflows",
                    "default_tool", "tools", "llm", "log", "hooks", "plugins",
                    "orca", "slack", "macos", "github", "herdr", "notion", "discord", "zeta"]
        var props: [String: JSONValue] = [:]
        for key in keys { props[key] = .object([:]) }
        let layout = SettingsLayout(schema: .object(["properties": .object(props)]))
        #expect(layout.general.keys == ["version", "max_concurrency", "worktree"])
        #expect(layout.settings.map(\.id) == ["repositories", "projects", "workflows", "tools", "llm", "log", "hooks"])
        #expect(layout.settings.first { $0.id == "tools" }?.keys == ["default_tool", "tools"])
        #expect(layout.plugins.map(\.id)
            == ["github", "notion", "slack", "discord", "herdr", "orca", "macos", "zeta"])
    }

    @Test func referencesOfferWhatIsConfigured() throws {
        let config = try #require(JSONValue.parse(#"""
            {"tools":{"claude-fast":{"kind":"claude"},"codex":{"kind":"codex"}},
             "plugins":{"herdr":{"kind":"agent_ide"},"github":{"kind":"task_source"},"orca":{"kind":"agent_ide"}},
             "projects":[{"name":"board"}],"repositories":[{"name":"web"},{"name":"cli"}]}
            """#))
        #expect(reference(for: ["workflows", "0", "tool"])?.0 == .tool)
        #expect(reference(for: ["workflows", "2", "projects"])?.multiple == true)
        #expect(reference(for: ["workflows", "0", "name"]) == nil)
        #expect(referenceOptions(.tool, config: config) == ["claude", "codex", "opencode", "claude-fast"])
        #expect(referenceOptions(.agent, config: config) == ["herdr", "orca"])
        #expect(referenceOptions(.source, config: config) == ["github"])
        #expect(referenceOptions(.project, config: config) == ["board"])
        #expect(referenceOptions(.repository, config: config) == ["web", "cli"])
    }
}

@Suite struct PlaceholderAndCleanupTests {
    @Test func placeholdersComeFromXPlaceholderOrDefault() throws {
        let explicit = try #require(JSONValue.parse(#"{"type":["integer","null"],"default":null,"x-placeholder":"4"}"#))
        #expect(placeholder(explicit) == "4")
        let derived = try #require(JSONValue.parse(#"{"type":"integer","default":30}"#))
        #expect(placeholder(derived) == "30")
        let none = try #require(JSONValue.parse(#"{"type":"string"}"#))
        #expect(placeholder(none) == nil)
    }

    @Test func cleanupIsAChoiceOrDays() throws {
        let cleanup = try #require(JSONValue.parse(#"""
            {"anyOf":[{"anyOf":[{"oneOf":[{"const":"immediate"},{"const":"manual"}]},
              {"type":"object","required":["retention_days"],"properties":{"retention_days":{"type":"integer"}}}]},
              {"type":"null"}],"x-title":"Cleanup"}
            """#))
        guard case .choiceOrObject(let choices, let object) = fieldKind(cleanup) else {
            Issue.record("not a choice-or-object: \(fieldKind(cleanup))")
            return
        }
        #expect(choices == ["immediate", "manual"])
        #expect(object["properties"]?["retention_days"] != nil)
    }

    @Test func decodesThePluginList() throws {
        let json = #"[{"name":"github","installed":true,"enabled":true,"kind":"task_source","version":"0.3.0"}]"#
        let list = try PluginInfo.decodeList(Data(json.utf8))
        #expect(list == [PluginInfo(name: "github", kind: "task_source", enabled: true)])
    }
}

@Suite struct FormHintsTests {
    @Test func longTextsAreMultiline() {
        #expect(isMultiline(["github", "prompts", "triage_instructions"], value: nil))
        #expect(isMultiline(["workflows", "0", "rubric"], value: nil))
        #expect(isMultiline(["x"], value: .string("a\nb")))
        #expect(!isMultiline(["log", "level"], value: .string("info")))
    }

    @Test func optionalFieldsWithADefaultAreAdvanced() throws {
        let withDefault = Property(key: "t", schema: .object(["default": .int(30)]), required: false)
        let bool = Property(key: "b", schema: .object(["type": .string("boolean"), "default": .bool(true)]), required: false)
        let required = Property(key: "r", schema: .object(["default": .int(1)]), required: true)
        let bare = Property(key: "s", schema: .object(["type": .string("string")]), required: false)
        #expect(isAdvanced(withDefault))
        #expect(isAdvanced(bool))
        #expect(!isAdvanced(required))
        #expect(!isAdvanced(bare))
    }
}
