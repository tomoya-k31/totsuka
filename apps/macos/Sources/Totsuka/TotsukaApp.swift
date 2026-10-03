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
        if let id = response.notification.request.content.userInfo["task_id"] as? String {
            Task { @MainActor in Self.model?.focus(id) }
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
                Image(systemName: status.symbol)
                    .foregroundStyle(status.color)
                    .font(.title3)
                Text(status.text)
                    .font(.headline)
                    .lineLimit(3)
                Spacer()
                toggle
            }
            if let notice = app.notice {
                Label(notice, systemImage: "info.circle").font(.callout)
            }
            if let error = app.menu?.error, app.runState == .running {
                Label(error, systemImage: "exclamationmark.circle").font(.callout)
            }
            ForEach(app.menu?.degraded ?? [], id: \.self) {
                Label($0, systemImage: "exclamationmark.triangle").font(.callout)
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
                // No settings window: config.toml is edited in $EDITOR, and the
                // log followed in $TERMINAL (ADR-0113 §5).
                Button { app.openSettings() } label: { Label("Settings…", systemImage: "gearshape") }
                    .help("Open config.toml in $EDITOR")
                Button { app.openLogs() } label: { Label("Logs", systemImage: "doc.text") }
                    .help("Follow totsuka run's output in $TERMINAL")
                Spacer()
                Menu {
                    Toggle("Open at login", isOn: Binding(
                        get: { app.launchesAtLogin }, set: { app.launchesAtLogin = $0 }))
                    Button("Forget saved secrets…") { app.forgetSecrets() }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                Button { app.quit() } label: { Label("Quit", systemImage: "power") }
            }
            .buttonStyle(.borderless)
        }
        .padding(14)
        .frame(width: 320)
    }

    /// One button for starting and stopping (and stopping now, once a stop is
    /// under way). Nothing to press while another `run` holds the lock.
    @ViewBuilder private var toggle: some View {
        switch app.runState {
        case .stopped, .failed:
            Button { Task { await app.start() } } label: {
                Label("Start", systemImage: "play.fill")
            }
            .buttonStyle(.borderedProminent)
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
                ? ("exclamationmark.circle.fill", .yellow, "Running (degraded)")
                : ("circle.fill", .green, "Running")
        case .stopping:
            return ("arrow.triangle.2.circlepath", .orange, "Stopping…")
        case .restarting(let at):
            return ("arrow.clockwise.circle", .orange,
                    "Restarting at " + at.formatted(date: .omitted, time: .standard))
        case .failed(let message):
            return ("exclamationmark.triangle.fill", .red, message)
        case .external:
            return ("circle.fill", .blue, "Running outside the app")
        }
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

/// A task row: focus, retry, cancel (no `verify` — it cannot be undone, as in
/// ADR-0065).
struct TaskMenu: View {
    @ObservedObject var app: AppModel
    let row: MenuRow

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 1) {
                Text("#\(row.taskId) \(row.title)").lineLimit(1)
                Text(row.detail()).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer()
            Menu {
                Button("Focus") { app.focus(String(row.taskId)) }
                Button("Retry") { app.retry(row.taskId) }
                Button("Cancel…") { app.cancel(row) }
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
        }
    }
}
