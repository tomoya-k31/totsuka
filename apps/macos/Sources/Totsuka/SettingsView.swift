import SwiftUI
import TotsukaKit

/// The settings window's state: the schema and the file, both through the CLI
/// (`config schema` / `config get` / `config set` / `config unset`, ADR-0109).
@MainActor
final class SettingsModel: ObservableObject {
    @Published private(set) var schema: JSONValue = .object([:])
    @Published private(set) var config: JSONValue = .object([:])
    @Published private(set) var configPath = ""
    @Published var error: String?
    @Published private(set) var validation: (ok: Bool, output: String)?
    @Published private(set) var loading = false

    let app: AppModel

    init(app: AppModel) {
        self.app = app
    }

    func reload() async {
        error = nil
        guard let cli = app.cli else {
            error = L("totsuka was not found", "totsuka が見つからない")
            return
        }
        loading = true
        defer { loading = false }
        do {
            let s = try await cli.run(["config", "schema"])
            guard s.status == 0 else { throw Message(s.errorMessage) }
            schema = try SchemaDocument.decode(s.stdout).schema
            try await reloadConfig()
        } catch {
            self.error = String(describing: error)
        }
    }

    private func reloadConfig() async throws {
        guard let cli = app.cli else { return }
        let g = try await cli.run(["config", "get"])
        guard g.status == 0 else { throw Message(g.errorMessage) }
        let doc = try ConfigDocument.decode(g.stdout)
        config = doc.config
        configPath = doc.configPath
    }

    /// Write one key and reload. The CLI refuses a write that would leave the
    /// file unreadable; its message is shown as is.
    func set(_ segments: [String], _ value: JSONValue) async {
        await write(["config", "set", JSONPointer.join(segments), value.jsonText])
    }

    func unset(_ segments: [String]) async {
        await write(["config", "unset", JSONPointer.join(segments)])
    }

    /// The writes run one at a time, in the order they were made: each is a
    /// read-modify-write of the whole file, and two at once would let the later
    /// one drop the earlier one's edit.
    private var lastWrite: Task<Void, Never>?

    private func write(_ arguments: [String]) async {
        let previous = lastWrite
        let task = Task { @MainActor in
            await previous?.value
            await self.perform(arguments)
        }
        lastWrite = task
        await task.value
    }

    private func perform(_ arguments: [String]) async {
        guard let cli = app.cli else { return }
        do {
            let r = try await cli.run(arguments)
            error = r.status == 0 ? nil : r.errorMessage
            try await reloadConfig()
        } catch {
            self.error = String(describing: error)
        }
    }

    /// A secret field: the value goes to the Keychain map and config.toml gets
    /// `secret:<name>` (ADR-0109 §5).
    func saveSecret(_ segments: [String], _ value: String) async {
        let name = secretName(for: segments)
        do {
            var map = try app.secrets.load()
            map[name] = value
            try app.secrets.save(map)
        } catch {
            self.error = String(describing: error)
            return
        }
        await set(segments, .string("secret:\(name)"))
    }

    /// `config validate --secrets-stdin`: the same gate `start` uses.
    func validate() async {
        guard let cli = app.cli, let map = app.loadSecrets() else { return }
        let r = try? await cli.run(
            ["config", "validate", "--secrets-stdin"], stdin: app.secretsLine(map))
        validation = (
            r?.status == 0,
            [r?.stdoutText, r?.stderrText].compactMap { $0 }.joined()
        )
    }

    /// The value at `segments` in the loaded file.
    func value(at segments: [String]) -> JSONValue? {
        var node: JSONValue? = config
        for segment in segments {
            if let index = Int(segment), let array = node?.array {
                node = index < array.count ? array[index] : nil
            } else {
                node = node?[segment]
            }
        }
        return node
    }

    /// The top-level keys grouped by `x-category`: the core's categories in
    /// the schema's order of first appearance, plugin tables after them.
    var categories: [(name: String, keys: [String])] {
        guard case .object(let props) = fieldKind(schema) else { return [] }
        var order: [String] = []
        var groups: [String: [String]] = [:]
        for prop in props.sorted(by: { $0.key < $1.key }) {
            let name =
                localized(nonNull(prop.schema), "x-category") ?? L("Other", "その他")
            if groups[name] == nil { order.append(name) }
            groups[name, default: []].append(prop.key)
        }
        return order.map { ($0, groups[$0]!) }
    }
}

struct Message: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

struct SettingsView: View {
    @ObservedObject var model: SettingsModel
    @ObservedObject var app: AppModel
    @State private var selection: String? = "general"

    var body: some View {
        NavigationSplitView {
            List(selection: $selection) {
                Label(L("General", "一般"), systemImage: "gearshape").tag("general")
                Label(L("Check", "確認"), systemImage: "checkmark.seal").tag("check")
                Section(L("Configuration", "設定")) {
                    ForEach(model.categories, id: \.name) { category in
                        Text(category.name).tag(category.name)
                    }
                }
            }
            .navigationSplitViewColumnWidth(min: 180, ideal: 200)
        } detail: {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if let error = model.error {
                        Text(error).foregroundStyle(.red).textSelection(.enabled)
                    }
                    detail
                }
                .padding()
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .frame(minWidth: 760, minHeight: 520)
        .task { await model.reload() }
    }

    @ViewBuilder private var detail: some View {
        switch selection {
        case "general", nil:
            GeneralSettings(app: app, configPath: model.configPath)
        case "check":
            CheckView(model: model)
        case let name?:
            if let keys = model.categories.first(where: { $0.name == name })?.keys,
                case .object(let props) = fieldKind(model.schema)
            {
                ForEach(props.filter { keys.contains($0.key) }) { prop in
                    FieldEditor(model: model, segments: [prop.key], schema: prop.schema, required: prop.required)
                }
            }
        }
    }
}

/// The app's own settings: where `totsuka` is, the `PATH` it runs with, and
/// the login item.
struct GeneralSettings: View {
    @ObservedObject var app: AppModel
    let configPath: String
    @AppStorage("totsukaPath") private var totsukaPath = ""
    @AppStorage("pathOverride") private var pathOverride = ""

    var body: some View {
        Form {
            LabeledContent(L("Config file", "設定ファイル"), value: configPath)
            TextField(L("totsuka path (empty: search PATH)", "totsuka のパス（空なら PATH から探す）"), text: $totsukaPath)
            TextField(L("PATH (empty: the login shell's)", "PATH（空ならログインシェルのもの）"), text: $pathOverride)
            Toggle(L("Open at login", "ログイン時に開く"), isOn: Binding(
                get: { app.launchesAtLogin }, set: { app.launchesAtLogin = $0 }))
            Text(L("Path changes take effect after restarting the app.", "パスの変更はアプリの再起動後に効く。"))
                .font(.caption).foregroundStyle(.secondary)
        }
    }
}

struct CheckView: View {
    @ObservedObject var model: SettingsModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L("Starting requires `totsuka config validate` to pass.",
                   "起動するには `totsuka config validate` が通る必要がある。"))
            Button(L("Check now", "確認する")) { Task { await model.validate() } }
            if let result = model.validation {
                Label(result.ok ? L("Passes", "通る") : L("Does not pass", "通らない"),
                      systemImage: result.ok ? "checkmark.circle" : "xmark.circle")
                    .foregroundStyle(result.ok ? .green : .red)
                Text(result.output).font(.system(.body, design: .monospaced)).textSelection(.enabled)
            }
        }
    }
}

/// One schema node, edited by its kind; recursive for tables and arrays.
struct FieldEditor: View {
    @ObservedObject var model: SettingsModel
    let segments: [String]
    let schema: JSONValue
    let required: Bool
    @State private var text = ""
    @State private var newKey = ""
    @State private var showingHelp = false

    private var node: JSONValue { nonNull(schema) }
    private var value: JSONValue? { model.value(at: segments) }
    private var title: String { localized(node, "x-title") ?? segments.last ?? "" }

    var body: some View {
        switch fieldKind(schema) {
        case .bool:
            Toggle(isOn: Binding(
                get: { value?.bool ?? node["default"]?.bool ?? false },
                set: { new in Task { await model.set(segments, .bool(new)) } })
            ) { label }
        case .integer, .number, .string:
            HStack {
                label
                TextField("", text: $text)
                    .onAppear { text = scalarText(value) }
                    .onChange(of: value) { _, new in text = scalarText(new) }
                    .onSubmit { Task { await commitScalar() } }
            }
        case .secret:
            HStack {
                label
                SecureField(secretPlaceholder, text: $text)
                    .onSubmit {
                        let entered = text
                        text = ""
                        Task { await model.saveSecret(segments, entered) }
                    }
            }
        case .choice(let choices):
            Picker(selection: Binding(
                get: { value?.string ?? "" },
                set: { new in Task {
                    if new.isEmpty { await model.unset(segments) } else { await model.set(segments, .string(new)) }
                } })
            ) {
                if !required { Text(L("(default)", "（既定）")).tag("") }
                ForEach(choices, id: \.self) { Text($0).tag($0) }
            } label: { label }
        case .stringList:
            HStack {
                label
                TextField(L("comma separated", "カンマ区切り"), text: $text)
                    .onAppear { text = listText(value) }
                    .onChange(of: value) { _, new in text = listText(new) }
                    .onSubmit { Task { await commitList() } }
            }
        case .object(let props):
            GroupBox {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(props) { prop in
                        AnyView(FieldEditor(model: model, segments: segments + [prop.key],
                                            schema: prop.schema, required: prop.required))
                    }
                }
            } label: { label }
        case .list(let item):
            GroupBox {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(Array((value?.array ?? []).enumerated()), id: \.offset) { index, element in
                        DisclosureGroup(element["name"]?.string ?? "#\(index + 1)") {
                            AnyView(FieldEditor(model: model, segments: segments + [String(index)],
                                                schema: item, required: true))
                            Button(L("Remove", "削除"), role: .destructive) {
                                Task { await model.unset(segments + [String(index)]) }
                            }
                        }
                    }
                    Button(L("Add", "追加")) {
                        Task { await model.set(segments + ["-"], newValue(for: item)) }
                    }
                }
            } label: { label }
        case .map(let valueSchema):
            GroupBox {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach((value?.object ?? [:]).keys.sorted(), id: \.self) { key in
                        DisclosureGroup(key) {
                            AnyView(FieldEditor(model: model, segments: segments + [key],
                                                schema: valueSchema, required: true))
                            Button(L("Remove", "削除"), role: .destructive) {
                                Task { await model.unset(segments + [key]) }
                            }
                        }
                    }
                    HStack {
                        TextField(L("name", "名前"), text: $newKey)
                        Button(L("Add", "追加")) {
                            let key = newKey
                            newKey = ""
                            Task { await model.set(segments + [key], newValue(for: valueSchema)) }
                        }
                        .disabled(newKey.isEmpty)
                    }
                }
            } label: { label }
        case .raw(let reason):
            VStack(alignment: .leading) {
                label
                if let reason { Text(reason).font(.caption).foregroundStyle(.secondary) }
                TextEditor(text: $text)
                    .font(.system(.body, design: .monospaced))
                    .frame(minHeight: 80)
                    .onAppear { text = (value ?? .object([:])).prettyText }
                Button(L("Save", "保存")) {
                    if let parsed = JSONValue.parse(text) {
                        Task { await model.set(segments, parsed) }
                    } else {
                        model.error = L("Not valid JSON", "JSON として正しくない")
                    }
                }
            }
        }
    }

    /// The title, a required mark, and the help popover (`x-help`).
    private var label: some View {
        HStack(spacing: 4) {
            Text(title + (required ? " *" : ""))
            if let help = localized(node, "x-help") {
                Button { showingHelp.toggle() } label: { Image(systemName: "questionmark.circle") }
                    .buttonStyle(.borderless)
                    .popover(isPresented: $showingHelp) {
                        Text(help).padding().frame(maxWidth: 320)
                    }
            }
        }
    }

    private var secretPlaceholder: String {
        switch value?.string {
        case let ref? where ref.hasPrefix("secret:"):
            return L("saved — type to replace", "保存済み — 入力すると置き換える")
        case .some:
            // `op://` and the like are refused under `--secrets-stdin`.
            return L("needs input (the current reference is not used by the app)",
                     "要入力（今の参照はアプリからは使えない）")
        case nil:
            return L("not set", "未設定")
        }
    }

    private func scalarText(_ value: JSONValue?) -> String {
        switch value {
        case .string(let s)?: return s
        case .int(let i)?: return String(i)
        case .double(let d)?: return String(d)
        default: return ""
        }
    }

    private func listText(_ value: JSONValue?) -> String {
        (value?.array ?? []).compactMap(\.string).joined(separator: ", ")
    }

    private func commitScalar() async {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty && !required {
            await model.unset(segments)
            return
        }
        switch fieldKind(schema) {
        case .integer:
            guard let i = Int64(trimmed) else {
                model.error = L("Not a whole number: ", "整数ではない: ") + trimmed
                return
            }
            await model.set(segments, .int(i))
        case .number:
            guard let d = Double(trimmed) else {
                model.error = L("Not a number: ", "数値ではない: ") + trimmed
                return
            }
            await model.set(segments, .double(d))
        default:
            await model.set(segments, .string(text))
        }
    }

    private func commitList() async {
        let items = text.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        if items.isEmpty && !required {
            await model.unset(segments)
        } else {
            await model.set(segments, .array(items.map(JSONValue.string)))
        }
    }
}
