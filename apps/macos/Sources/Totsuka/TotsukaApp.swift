import AppKit
import SwiftUI
import TotsukaKit
import UserNotifications

@main
struct TotsukaApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var app: AppModel

    init() {
        let app = AppModel()
        _app = StateObject(wrappedValue: app)
        AppDelegate.model = app
    }

    var body: some Scene {
        MenuBarExtra {
            MenuContent(app: app)
        } label: {
            MenuLabel(app: app)
        }
        .menuBarExtraStyle(.window)
    }
}

/// Notification clicks: bring the task's agent pane to the front (F-94).
final class AppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    @MainActor static var model: AppModel?

    func applicationDidFinishLaunching(_ notification: Notification) {
        UNUserNotificationCenter.current().delegate = self
    }

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { Self.model?.willTerminate() }
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let info = response.notification.request.content.userInfo
        if let id = info["task_id"] as? String {
            let workflow = (info["workflow"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            Task { @MainActor in Self.model?.focus(id, workflow: workflow) }
        }
        completionHandler()
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification,
        withCompletionHandler completionHandler:
            @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .sound])
    }
}

/// The menu bar item: the template icon alone (the asset catalog's; an SF
/// Symbol in a development build without one). The tasks waiting on you are
/// listed in the panel, not counted beside the icon: it blinks while there are
/// any, and a glint runs up the blade while a task is working.
struct MenuLabel: View {
    @ObservedObject var app: AppModel

    /// A glint running up the blade, then a pause. Frame by frame rather than
    /// a symbol effect, so the custom icon itself moves; each frame wakes the
    /// app, so keep the rate low (4 fps, `AppModel.iconTick`).
    private static let frames: [NSImage] = (1...8).compactMap {
        NSImage(named: "StatusBarWorking\($0)Template")
    }

    var body: some View {
        let needsYou = !(app.menu?.attention.isEmpty ?? true)
        let working = !(app.menu?.working.isEmpty ?? true)
        let tick = app.iconTick
        // Waiting on you outranks working: blink the still icon.
        let frame = !needsYou && working && !Self.frames.isEmpty
            ? Self.frames[tick % Self.frames.count]
            : NSImage(named: "StatusBarTemplate")
                ?? NSImage(systemSymbolName: "bolt.circle", accessibilityDescription: nil)
        if let frame {
            // Half a second on, half a second faint. Dimmed while no `run` is
            // running, so the state reads at a glance; an outside `run`
            // (another terminal) counts as running.
            Image(nsImage: Self.faded(frame,
                (needsYou && tick % 4 >= 2 ? 0.2 : 1)
                    * (app.runState == .running || app.runState == .external ? 1 : 0.45)))
        } else {
            Image(systemName: "bolt.circle")
        }
    }

    /// `image` drawn at `alpha`, baked into the image itself: the label's
    /// `.opacity` does not reach the status bar button.
    private static func faded(_ image: NSImage, _ alpha: CGFloat) -> NSImage {
        guard alpha < 1 else { return image }
        let faded = NSImage(size: image.size, flipped: false) { rect in
            image.draw(in: rect, from: .zero, operation: .sourceOver, fraction: alpha)
            return true
        }
        faded.isTemplate = true
        return faded
    }
}

/// The menu bar panel: a fixed-width window rather than a plain menu, whose
/// width would follow its shortest labels.
struct MenuContent: View {
    @ObservedObject var app: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                if app.runState == .starting || app.runState == .stopping {
                    // A spinner rather than a still symbol: it says "in progress".
                    ProgressView().controlSize(.small).frame(width: 16, height: 16)
                } else {
                    Image(systemName: status.symbol)
                        .foregroundStyle(status.color)
                        .font(.headline)
                }
                Text(status.text)
                    .font(.headline)
                    .lineLimit(3)
                    // Wrap rather than truncate: the panel sizes to one line otherwise.
                    .fixedSize(horizontal: false, vertical: true)
                Spacer()
                ForEach(app.agentIDEs, id: \.name) { ide in
                    HStack(spacing: 2) {
                        Image(systemName: ide.up ? "checkmark.circle.fill" : "xmark.circle.fill")
                            .foregroundStyle(ide.up ? .green : .red)
                        Text(ide.name)
                    }
                    // Same size as the status beside it; the weight tells them apart.
                    .font(.body)
                    .fixedSize()
                    .help(ide.up ? "\(ide.name) is running" : "\(ide.name) is not running → start it")
                }
                toggle
            }
            if app.runState == .stopped {
                Text("Start to pick up tasks and hand them to your agents.")
                    .font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if case .failed(let message) = app.runState {
                FailureBox(message: message)
            }
            if let notice = app.notice {
                Label(notice, systemImage: "info.circle").font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let error = app.menu?.error, app.runState == .running {
                Label(error, systemImage: "exclamationmark.circle").font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
            }
            ForEach(app.menu?.degraded ?? [], id: \.self) {
                Label($0, systemImage: "exclamationmark.triangle").font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let rows = app.menu?.attention, !rows.isEmpty {
                TaskSection(app: app, title: "Needs you", rows: rows)
            }
            if let rows = app.menu?.working, !rows.isEmpty {
                TaskSection(app: app, title: "Working", rows: rows)
            }
            Divider()
            if app.needsRestart {
                Button("Updated — restart") { app.restartIntoUpdate() }
            }
            HStack {
                // A development build has no version (see `checkVersion`).
                if !app.appVersion.isEmpty {
                    Text("v\(app.appVersion)").font(.caption2).foregroundStyle(.tertiary)
                }
                Spacer()
                Menu {
                    // No settings window: config.toml is edited in $EDITOR, and
                    // the log followed in $TERMINAL (ADR-0113 §5).
                    Button { app.openSettings() } label: { Label("Settings…", systemImage: "gearshape") }
                        .help("Open config.toml in $EDITOR")
                    Button { app.openLogs() } label: { Label("Logs", systemImage: "doc.text") }
                        .help("Follow totsuka run's output in $TERMINAL")
                    Divider()
                    Toggle(isOn: Binding(
                        get: { app.launchesAtLogin }, set: { app.launchesAtLogin = $0 })
                    ) { Label("Open at login", systemImage: "person.badge.clock") }
                    Button { app.forgetSecrets() } label: {
                        Label("Forget saved secrets…", systemImage: "key.slash")
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                // Menu items drop a Label's icon unless asked for it.
                .labelStyle(.titleAndIcon)
                .menuStyle(.borderlessButton)
                .fixedSize()
                Button { app.quit() } label: { Label("Quit", systemImage: "power") }
            }
            .buttonStyle(.borderless)
        }
        .padding(14)
        .frame(width: 400)
    }

    /// One button for starting and stopping (and stopping now, once a stop is
    /// under way). Nothing to press while another `run` holds the lock.
    @ViewBuilder private var toggle: some View {
        switch app.runState {
        case .stopped, .failed:
            Button { Task { await app.start() } } label: {
                Label("Start", systemImage: "play.fill")
            }
            .buttonStyle(.bordered)
        case .running, .starting, .restarting:
            Button { app.stop() } label: { Label("Stop", systemImage: "stop.fill") }
                .buttonStyle(.bordered)
        case .stopping:
            Button { app.forceStop() } label: {
                Label("Stop now", systemImage: "xmark.octagon")
            }
            .buttonStyle(.bordered)
        case .external:
            EmptyView()
        }
    }

    private var status: (symbol: String, color: Color, text: String) {
        switch app.runState {
        case .stopped:
            return ("circle", .secondary, "Stopped")
        case .starting:
            return ("arrow.triangle.2.circlepath", .orange, "Starting…")
        case .running:
            return app.menu?.availability == "degraded"
                ? ("exclamationmark.circle.fill", .yellow, "Degraded")
                : ("circle.fill", .green, "Running")
        case .stopping:
            return ("arrow.triangle.2.circlepath", .orange, "Stopping…")
        case .restarting(let at):
            return ("arrow.clockwise.circle", .orange,
                    "Restarting at " + at.formatted(date: .omitted, time: .standard))
        case .failed:
            return ("exclamationmark.triangle.fill", .red, "Failed")
        case .external:
            return ("circle.fill", .blue, "Running outside the app")
        }
    }
}

/// A failed start's message in full: wrapped, scrollable past 180 pt, and
/// selectable so it can be copied (the header used to cut it at three lines).
/// The box is as tall as the text, so a one-line error leaves no blank space.
struct FailureBox: View {
    let message: String
    @State private var textHeight: CGFloat = 0

    var body: some View {
        ScrollView {
            Text(message)
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(8)
                .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { textHeight = $0 }
        }
        .frame(height: textHeight > 0 ? min(textHeight, 180) : nil)
        .background(Color.red.opacity(0.12), in: RoundedRectangle(cornerRadius: 6))
    }
}

struct TaskSection: View {
    @ObservedObject var app: AppModel
    let title: String
    let rows: [MenuRow]

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            ForEach(rows) { TaskMenu(app: app, row: $0) }
        }
    }
}

/// A task row: a click focuses it; retry and cancel in its menu (no `verify`
/// — it cannot be undone, as in ADR-0065).
struct TaskMenu: View {
    @ObservedObject var app: AppModel
    let row: MenuRow
    @State private var hovering = false

    var body: some View {
        HStack {
            Button { app.focus(String(row.taskId), workflow: row.workflow) } label: {
                HStack {
                    VStack(alignment: .leading, spacing: 1) {
                        Text("#\(row.taskId) \(row.title)").lineLimit(1)
                        Text(row.detail()).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                    Spacer()
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Focus")
            Menu {
                Button("Retry") { app.retry(row.taskId) }
                Button("Cancel…") { app.cancel(row) }
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
        }
        .padding(.horizontal, 4)
        .background(RoundedRectangle(cornerRadius: 4).fill(hovering ? Color.primary.opacity(0.08) : .clear))
        .onHover { hovering = $0 }
    }
}
