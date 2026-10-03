import Foundation

/// The secret names config.toml refers to (`secret:<name>`, ADR-0100),
/// without repeats, in the order a walk with sorted keys meets them (a JSON
/// object keeps no order, so this is what makes the questions' order stable)
/// — what the app asks for before `run` starts when the Keychain map lacks
/// one. A name outside the CLI's alphabet (`[A-Za-z0-9_.-]`, ADR-0100) is
/// skipped: `config validate` refuses it anyway, and asking for it would only
/// store a value nothing can ever read.
public func secretNames(in config: JSONValue) -> [String] {
    var names: [String] = []
    func walk(_ value: JSONValue) {
        switch value {
        case .string(let s):
            if s.hasPrefix("secret:") {
                let name = String(s.dropFirst("secret:".count))
                if isSecretName(name), !names.contains(name) { names.append(name) }
            }
        case .array(let items):
            items.forEach(walk)
        case .object(let map):
            map.keys.sorted().forEach { walk(map[$0]!) }
        default:
            break
        }
    }
    walk(config)
    return names
}

/// Whether config.toml names any `secret:` value — then `run` is given the
/// Keychain map with `--secrets-stdin`; otherwise it is started without, and
/// resolves `op://`, `cmd:`, `bw:`, `keychain:` itself, as from a terminal.
/// (The two cannot be mixed: under `--secrets-stdin` every other store is
/// refused, ADR-0100.) Any `secret:` string counts, valid name or not, so a
/// misspelt one still fails `config validate` instead of being skipped.
public func usesSuppliedSecrets(_ config: JSONValue) -> Bool {
    switch config {
    case .string(let s): return s.hasPrefix("secret:")
    case .array(let items): return items.contains(where: usesSuppliedSecrets)
    case .object(let map): return map.values.contains(where: usesSuppliedSecrets)
    default: return false
    }
}

private func isSecretName(_ name: String) -> Bool {
    !name.isEmpty
        && name.unicodeScalars.allSatisfy {
            $0.isASCII && (CharacterSet.alphanumerics.contains($0) || "_.-".unicodeScalars.contains($0))
        }
}

/// `s` as one `/bin/sh` word.
public func shellQuote(_ s: String) -> String {
    "'" + s.replacingOccurrences(of: "'", with: #"'\''"#) + "'"
}

/// The `/bin/sh -c` line that opens `path` in `$EDITOR` inside `$TERMINAL`
/// (`$TERMINAL -e $EDITOR <path>`), or nil when either is unset. Both are the
/// user's own shell fragments and may carry arguments (`nvim -p`,
/// `alacritty --class x`), so they are left unquoted; the path is quoted.
public func editorCommand(environment: [String: String], path: String) -> String? {
    guard let terminal = nonEmpty(environment["TERMINAL"]),
        let editor = nonEmpty(environment["EDITOR"])
    else { return nil }
    return "exec \(terminal) -e \(editor) \(shellQuote(path))"
}

/// The `/bin/sh -c` line that follows `path` in `$TERMINAL`
/// (`tail -n 200 -F`), or nil when `$TERMINAL` is unset.
public func tailCommand(environment: [String: String], path: String) -> String? {
    guard let terminal = nonEmpty(environment["TERMINAL"]) else { return nil }
    return "exec \(terminal) -e tail -n 200 -F \(shellQuote(path))"
}

private func nonEmpty(_ value: String?) -> String? {
    guard let value = value?.trimmingCharacters(in: .whitespaces), !value.isEmpty else {
        return nil
    }
    return value
}

/// The supervised `run`'s stderr, kept in a file for `tail -F`:
/// `$XDG_STATE_HOME/totsuka/app-run.log`. Each start appends a header line;
/// a file grown past `limit` bytes is started over at the next start.
public struct RunLogFile: Sendable {
    public let url: URL

    public init(stateDirectory: URL) {
        url = stateDirectory.appendingPathComponent("app-run.log")
    }

    /// Mark a new run (and start the file over when it is too large).
    public func begin(at date: Date = Date(), limit: Int = 5_000_000) {
        let fm = FileManager.default
        try? fm.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let size = (try? fm.attributesOfItem(atPath: url.path)[.size] as? Int) ?? 0
        if size > limit { try? fm.removeItem(at: url) }
        append("--- run started \(date.formatted(.iso8601)) ---")
    }

    /// One line of stderr. A write that fails is dropped: the log is for
    /// reading along, never a reason to stop `run`.
    public func append(_ line: String) {
        let data = Data((line + "\n").utf8)
        if let handle = try? FileHandle(forWritingTo: url) {
            defer { try? handle.close() }
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: data)
        } else {
            try? FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try? data.write(to: url)
        }
    }
}
