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

@Suite struct CLITests {
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

@Suite struct LaunchTests {
    @Test func findsEverySecretReferenceOnce() throws {
        let config = try #require(JSONValue.parse(#"""
        {"github":{"token":"secret:github.token"},
         "llm":{"api_key_ref":"op://v/i/f"},
         "slack":{"app_token":"secret:slack.app","bot_token":"secret:github.token"},
         "x":["secret:", "secret:in.list", "secret:bad name", "secret:日本"]}
        """#))
        #expect(secretNames(in: config) == ["github.token", "slack.app", "in.list"])
    }

    /// Only a config that names a `secret:` value is run with
    /// `--secrets-stdin`; `op://` / `cmd:` alone are left to `run`.
    @Test func suppliedSecretsOnlyWhenTheConfigNamesOne() throws {
        let plain = try #require(JSONValue.parse(#"""
        {"github":{"token":"cmd:gh auth token"},"llm":{"api_key_ref":"op://v/i/f"},"n":4}
        """#))
        #expect(!usesSuppliedSecrets(plain))
        let nested = try #require(JSONValue.parse(#"{"tools":{"x":{"env":["secret:bad name"]}}}"#))
        #expect(usesSuppliedSecrets(nested), "a misspelt name still counts")
    }

    @Test func buildsTerminalCommandsFromTheEnvironment() {
        let env = ["TERMINAL": "alacritty", "EDITOR": "nvim -p"]
        #expect(
            editorCommand(environment: env, path: "/a b/it's.toml")
                == #"exec alacritty -e nvim -p '/a b/it'\''s.toml'"#)
        #expect(
            tailCommand(environment: env, path: "/s/app-run.log")
                == "exec alacritty -e tail -n 200 -F '/s/app-run.log'")
        #expect(editorCommand(environment: ["TERMINAL": "alacritty"], path: "/c") == nil)
        #expect(editorCommand(environment: ["EDITOR": "vim", "TERMINAL": " "], path: "/c") == nil)
        #expect(tailCommand(environment: [:], path: "/c") == nil)
    }

    @Test func runLogStartsOverWhenTooLarge() throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        let log = RunLogFile(stateDirectory: dir)
        log.begin()
        log.append("one")
        var text = try String(contentsOf: log.url, encoding: .utf8)
        #expect(text.hasPrefix("--- run started "))
        #expect(text.hasSuffix("one\n"))
        log.begin(limit: 1)
        text = try String(contentsOf: log.url, encoding: .utf8)
        #expect(!text.contains("one"))
        try? FileManager.default.removeItem(at: dir)
    }
}

@Suite struct MenuRowTests {
    @Test func detailShowsRepoWorkflowStateAndElapsed() throws {
        let data = Data(#"""
        {"availability":"ok","attention_count":0,"attention":[],"degraded":[],
         "working":[{"task_id":97,"state":"running","workflow":"github-task","title":"t",
                     "repo":"web","created_at":"2026-10-03T11:48:10.699617Z"},
                    {"task_id":98,"state":"queued","workflow":"w","title":"t"}]}
        """#.utf8)
        let menu = try MenuModel.decode(data)
        let now = try #require(parseTimestamp("2026-10-03T12:00:40Z"))
        #expect(menu.working[0].detail(now: now) == "web · github-task · running · 12m")
        // An older CLI sends neither field: the row still renders.
        #expect(menu.working[1].detail(now: now) == "w · queued")
    }

    @Test func elapsedIsShortAndNeverNegative() {
        let start = Date(timeIntervalSince1970: 0)
        #expect(elapsedText(from: start, to: start.addingTimeInterval(45)) == "45s")
        #expect(elapsedText(from: start, to: start.addingTimeInterval(3 * 3600 + 5 * 60)) == "3h 5m")
        #expect(elapsedText(from: start, to: start.addingTimeInterval(2 * 86400 + 4 * 3600)) == "2d 4h")
        #expect(elapsedText(from: start, to: start.addingTimeInterval(-5)) == "0s")
    }
}
