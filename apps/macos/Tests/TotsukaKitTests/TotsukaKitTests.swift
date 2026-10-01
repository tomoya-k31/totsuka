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
                  "name":{"type":"string","x-title":{"en":"Name","ja":"名前"}},
                  "level":{"type":["string","null"]},
                  "mode":{"anyOf":[{"oneOf":[{"const":"plan"},{"const":"implement"}]},{"type":"null"}],
                          "x-title":{"en":"Mode","ja":"モード"}},
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
        #expect(localized(nonNull(mode), "x-title", language: "ja") == "モード")
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
        #expect(secretName(for: ["tools", "a/b c"]) == "tools.a_b_c")
        #expect(JSONPointer.join(["tools", "my.tool", "a/b"]) == "/tools/my.tool/a~1b")
    }

    @Test func integersSurviveARoundTrip() throws {
        let value = try #require(JSONValue.parse(#"{"n":4,"r":0.8}"#))
        #expect(value.jsonText == #"{"n":4,"r":0.8}"#)
    }

    @Test func parsesEnvOutput() {
        let env = parseEnv("PATH=/a:/b\nHOME=/h\nMULTI=line1\nline2\n=bad\n")
        #expect(env["PATH"] == "/a:/b")
        #expect(env["MULTI"] == "line1")
        #expect(env.count == 3)
    }
}
