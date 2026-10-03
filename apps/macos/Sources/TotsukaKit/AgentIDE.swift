import Foundation

/// The agent IDEs `[plugins]` enables that the menu checks: herdr and orca.
/// Plugin instance names are their binary names, so the name is the type.
public func enabledAgentIDEs(in config: JSONValue) -> [String] {
    ["herdr", "orca"].filter { config["plugins"]?[$0]?["enabled"]?.bool == true }
}

/// herdr's socket, resolved as agent-ide-herdr resolves it: `[herdr].socket_path`,
/// `[herdr].session`, `HERDR_SOCKET_PATH`, `HERDR_SESSION`, then the default.
public func herdrSocketPath(config: JSONValue, environment: [String: String]) -> String {
    let xdg = environment["XDG_CONFIG_HOME"].flatMap { $0.isEmpty ? nil : $0 }
    let dir = (xdg ?? (environment["HOME"] ?? NSHomeDirectory()) + "/.config") + "/herdr"
    func session(_ name: String) -> String { "\(dir)/sessions/\(name)/herdr.sock" }
    if let path = config["herdr"]?["socket_path"]?.string {
        return (path as NSString).expandingTildeInPath
    }
    if let name = config["herdr"]?["session"]?.string { return session(name) }
    if let path = environment["HERDR_SOCKET_PATH"] { return path }
    if let name = environment["HERDR_SESSION"] { return session(name) }
    return dir + "/herdr.sock"
}

/// Whether something accepts connections on the Unix socket at `path` — a
/// socket file left by a crashed herdr does not.
public func unixSocketAccepts(_ path: String) -> Bool {
    let fd = socket(AF_UNIX, SOCK_STREAM, 0)
    guard fd >= 0 else { return false }
    defer { close(fd) }
    var address = sockaddr_un()
    address.sun_family = sa_family_t(AF_UNIX)
    let capacity = MemoryLayout.size(ofValue: address.sun_path)
    guard path.utf8.count < capacity else { return false }
    withUnsafeMutableBytes(of: &address.sun_path) { buffer in
        buffer.copyBytes(from: path.utf8)
        buffer[path.utf8.count] = 0
    }
    return withUnsafePointer(to: &address) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) == 0
        }
    }
}

/// `orca status --json`: whether the runtime behind the CLI is up. The CLI
/// answers even with the app closed, so only `runtime.reachable` tells.
public func orcaRuntimeReachable(_ stdout: Data) -> Bool {
    JSONValue.parse(String(decoding: stdout, as: UTF8.self))?["result"]?["runtime"]?["reachable"]?
        .bool ?? false
}

/// The GUI app that shows a task's pane, to bring forward on focus.
public enum FocusApp: Equatable, Sendable {
    case bundleID(String)
    case named(String)
}

/// orca's own window for an orca workflow; otherwise the terminal herdr runs
/// in, `[macos].activate_bundle_id` (the one click-to-focus already uses).
/// An unknown workflow gets the terminal too.
public func focusApp(workflow: String?, config: JSONValue) -> FocusApp? {
    let agent = config["workflows"]?.array?.first { $0["name"]?.string == workflow }?["agent"]?
        .string
    if agent == "orca" { return .named("Orca") }
    return config["macos"]?["activate_bundle_id"]?.string.flatMap { $0.isEmpty ? nil : .bundleID($0) }
}
