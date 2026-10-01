import AppKit
import SwiftUI
import TotsukaKit
import UserNotifications

@main
struct TotsukaApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var app: AppModel
    @StateObject private var settings: SettingsModel

    init() {
        let app = AppModel()
        _app = StateObject(wrappedValue: app)
        _settings = StateObject(wrappedValue: SettingsModel(app: app))
        AppDelegate.model = app
    }

    var body: some Scene {
        MenuBarExtra {
            MenuContent(app: app)
        } label: {
            MenuLabel(app: app)
        }
        Settings {
            SettingsView(model: settings, app: app)
        }
        Window(L("Totsuka Logs", "Totsuka のログ"), id: "logs") {
            LogsView(app: app)
        }
    }
}

/// Notification clicks: bring the task's agent pane to the front (F-94).
final class AppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    @MainActor static var model: AppModel?

    func applicationDidFinishLaunching(_ notification: Notification) {
        UNUserNotificationCenter.current().delegate = self
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

/// The menu bar item: the template icon (the asset catalog's; an SF Symbol in
/// a development build without one) and the number of tasks waiting on you.
struct MenuLabel: View {
    @ObservedObject var app: AppModel

    var body: some View {
        HStack(spacing: 2) {
            if let image = NSImage(named: "StatusBarTemplate") {
                Image(nsImage: image)
            } else {
                Image(systemName: "bolt.circle")
            }
            if let count = app.menu?.attentionCount, count > 0 {
                Text("\(count)")
            }
        }
    }
}

struct MenuContent: View {
    @ObservedObject var app: AppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Text(statusText)
        if let notice = app.notice { Text(notice) }
        if let error = app.menu?.error, app.runState == .running { Text(error) }
        ForEach(app.menu?.degraded ?? [], id: \.self) { Text("⚠ " + $0) }

        switch app.runState {
        case .stopped, .failed, .restarting:
            Button(L("Start", "起動")) { Task { await app.start() } }
        case .running, .starting:
            Button(L("Stop", "停止")) { app.stop() }
        case .stopping:
            Button(L("Stop now", "すぐに停止")) { app.forceStop() }
        case .external:
            EmptyView()
        }

        if let rows = app.menu?.attention, !rows.isEmpty {
            Divider()
            Text(L("Needs you", "要対応"))
            ForEach(rows) { TaskMenu(app: app, row: $0) }
        }
        if let rows = app.menu?.working, !rows.isEmpty {
            Divider()
            Text(L("Working", "作業中"))
            ForEach(rows) { TaskMenu(app: app, row: $0) }
        }

        Divider()
        if app.needsRestart {
            Button(L("Updated — restart", "更新済み・再起動")) { app.restartIntoUpdate() }
        }
        SettingsLink { Text(L("Settings…", "設定…")) }
        Button(L("Logs", "ログ")) {
            openWindow(id: "logs")
            NSApp.activate()
        }
        Button(L("Quit", "終了")) { app.quit() }
    }

    private var statusText: String {
        switch app.runState {
        case .stopped: return L("Stopped", "停止中")
        case .starting: return L("Starting…", "起動中…")
        case .running: return app.menu?.availability == "degraded"
            ? L("Running (degraded)", "稼働中（縮退）") : L("Running", "稼働中")
        case .stopping: return L("Stopping…", "停止中…")
        case .restarting(let at):
            return L("Restarting at ", "再起動予定: ") + at.formatted(date: .omitted, time: .standard)
        case .failed(let message): return message
        case .external: return L("Running outside the app", "アプリの外で稼働中")
        }
    }
}

/// A task row: focus, retry, cancel (no `verify` — it cannot be undone, as in
/// ADR-0065).
struct TaskMenu: View {
    @ObservedObject var app: AppModel
    let row: MenuRow

    var body: some View {
        Menu("#\(row.taskId) \(row.title)") {
            Text("\(row.workflow) · \(row.state)")
            Button(L("Focus", "前面に出す")) { app.focus(String(row.taskId)) }
            Button(L("Retry", "やり直す")) { app.retry(row.taskId) }
            Button(L("Cancel…", "取り消す…")) { app.cancel(row) }
        }
    }
}

struct LogsView: View {
    @ObservedObject var app: AppModel

    var body: some View {
        VStack(alignment: .leading) {
            ScrollView {
                Text(app.logLines.joined(separator: "\n"))
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            Button(L("Open log folder", "ログのフォルダを開く")) {
                let dir = stateDirectory(environment: app.cli?.environment ?? [:])
                    .appendingPathComponent("logs")
                NSWorkspace.shared.open(dir)
            }
        }
        .padding()
        .frame(minWidth: 600, minHeight: 360)
    }
}
