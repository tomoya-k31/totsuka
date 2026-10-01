import Foundation

/// One supervised `totsuka run --watch --secrets-stdin --events-jsonl`
/// (ADR-0109 §2). Owns the child, feeds it the secret map on stdin and keeps
/// stdin open, and turns its stdout into [`RunEvent`]s and its stderr into
/// log lines. Restart decisions are the caller's ([`exitDecision`]).
public final class RunProcess {
    public struct Termination: Sendable {
        public let status: Int32
        public let bySignal: Bool
    }

    private let process = Process()
    private let stdin = Pipe()
    private let stdout = Pipe()
    private let stderr = Pipe()

    /// - Parameters:
    ///   - onEvent: each `run --events-jsonl` line, on a background queue.
    ///   - onLog: each stderr line, on a background queue.
    ///   - onExit: once, when the process has exited.
    public init(
        cli: TotsukaCLI,
        onEvent: @escaping (RunEvent) -> Void,
        onLog: @escaping (String) -> Void,
        onExit: @escaping (Termination) -> Void
    ) {
        process.executableURL = cli.binary
        process.arguments = ["run", "--watch", "--secrets-stdin", "--events-jsonl"]
        process.environment = cli.environment
        process.standardInput = stdin
        process.standardOutput = stdout
        process.standardError = stderr
        // `onExit` waits for both pipes to reach EOF, so the last stderr lines
        // (the reason for an exit 4) are delivered before the exit is.
        let drained = DispatchGroup()
        drained.enter()
        drained.enter()
        Self.readLines(stdout, done: { drained.leave() }) { line in
            if let event = RunEvent.parse(line) { onEvent(event) } else { onLog(line) }
        }
        Self.readLines(stderr, done: { drained.leave() }, onLog)
        process.terminationHandler = { process in
            let termination = Termination(
                status: process.terminationStatus,
                bySignal: process.terminationReason == .uncaughtSignal)
            drained.notify(queue: .global()) { onExit(termination) }
        }
    }

    /// Start the child and hand it the secrets: one JSON object on one line.
    /// stdin stays open afterwards — `run` reads the first line only, and
    /// lines after it are reserved (ADR-0100).
    public func start(secrets: [String: String]) throws {
        try process.run()
        var line = (try? JSONEncoder().encode(secrets)) ?? Data("{}".utf8)
        line.append(UInt8(ascii: "\n"))
        stdin.fileHandleForWriting.write(line)
    }

    /// Ask for a graceful stop (SIGTERM; `run` drains and exits 0).
    public func terminate() {
        if process.isRunning { process.terminate() }
    }

    /// Stop it now (SIGKILL), for a stop that has waited long enough.
    public func kill() {
        if process.isRunning { Darwin.kill(process.processIdentifier, SIGKILL) }
    }

    public var isRunning: Bool { process.isRunning }

    /// Split a pipe into lines as they arrive.
    private static func readLines(
        _ pipe: Pipe, done: @escaping () -> Void, _ handle: @escaping (String) -> Void
    ) {
        var buffer = Data()
        let lock = NSLock()
        pipe.fileHandleForReading.readabilityHandler = { file in
            let chunk = file.availableData
            lock.lock()
            defer { lock.unlock() }
            if chunk.isEmpty {
                file.readabilityHandler = nil
                if !buffer.isEmpty { handle(String(decoding: buffer, as: UTF8.self)) }
                buffer.removeAll()
                done()
                return
            }
            buffer.append(chunk)
            while let newline = buffer.firstIndex(of: UInt8(ascii: "\n")) {
                let line = buffer[buffer.startIndex..<newline]
                buffer.removeSubrange(buffer.startIndex...newline)
                handle(String(decoding: line, as: UTF8.self))
            }
        }
    }
}
