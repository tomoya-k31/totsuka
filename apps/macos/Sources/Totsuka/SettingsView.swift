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
    /// `totsuka plugin list --json`: the installed plugins and their kinds,
    /// for the sidebar's enable switches.
    @Published private(set) var plugins: [PluginInfo] = []

    let app: AppModel

    init(app: AppModel) {
        self.app = app
    }

    func reload() async {
        error = nil
        guard let cli = app.cli else {
            error = "totsuka was not found"
            return
        }
        do {
            let s = try await cli.run(["config", "schema"])
            guard s.status == 0 else { throw Message(s.errorMessage) }
            schema = try SchemaDocument.decode(s.stdout).schema
            if let list = try? await cli.run(["plugin", "list", "--json"]), list.status == 0 {
                plugins = (try? PluginInfo.decodeList(list.stdout)) ?? []
            }
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

    var layout: SettingsLayout { SettingsLayout(schema: schema) }

    // MARK: plugins

    func isEnabled(_ plugin: String) -> Bool {
        config["plugins"]?[plugin]?["enabled"]?.bool ?? false
    }

    /// The sidebar switch: `[plugins.<name>].enabled`, creating the entry
    /// (with the installed plugin's kind) the first time it is turned on.
    func setEnabled(_ plugin: String, _ on: Bool) async {
        if config["plugins"]?[plugin] != nil {
            await set(["plugins", plugin, "enabled"], .bool(on))
        } else if on {
            guard let kind = plugins.first(where: { $0.name == plugin })?.kind else {
                error = "\(plugin) is not installed"
                return
            }
            await set(["plugins", plugin], .object(["kind": .string(kind), "enabled": .bool(true)]))
        }
    }

    /// The schema of one `[plugins.<name>]` entry.
    var rosterEntrySchema: JSONValue? {
        guard let roster = property("plugins")?.schema,
            case .map(let value) = fieldKind(roster)
        else { return nil }
        return value
    }

    // MARK: collections (repositories, projects, workflows, tools)

    /// The schema of one element of a collection page's key.
    func elementSchema(_ key: String) -> JSONValue? {
        guard let prop = property(key) else { return nil }
        switch fieldKind(prop.schema) {
        case .list(let item): return item
        case .map(let value): return value
        default: return nil
        }
    }

    /// The sidebar children of a collection: `(id, title)`, where `id` is
    /// `<key>/<index or name>`.
    func children(of key: String) -> [(id: String, title: String)] {
        if let array = config[key]?.array {
            return array.enumerated().map { index, element in
                let name = element["name"]?.string ?? ""
                return ("\(key)/\(index)", name.isEmpty ? "(unnamed)" : name)
            }
        }
        return (config[key]?.object ?? [:]).keys.sorted().map { ("\(key)/\($0)", $0) }
    }

    /// Append a new element (only its required keys) and return its id.
    func addElement(to key: String) async -> String? {
        guard let schema = elementSchema(key) else { return nil }
        let index = config[key]?.array?.count ?? 0
        await set([key, "-"], newValue(for: schema))
        return error == nil ? "\(key)/\(index)" : nil
    }

    /// Add a named entry to a table of entries (`[tools.<name>]`).
    func addEntry(to key: String, named name: String) async -> String? {
        guard let schema = elementSchema(key) else { return nil }
        await set([key, name], newValue(for: schema))
        return error == nil ? "\(key)/\(name)" : nil
    }

    /// The schema of a top-level key.
    func property(_ key: String) -> Property? {
        guard case .object(let props) = fieldKind(schema) else { return nil }
        return props.first { $0.key == key }
    }

    /// A page's sidebar title.
    func title(of page: SettingsPage) -> String {
        if page.app { return "General" }
        let key = page.keys.contains(page.id) ? page.id : page.keys[0]
        let node = property(key).map { nonNull($0.schema) } ?? .null
        // A plugin's table has no `x-title` of its own; its category is its name.
        return annotation(node, "x-title") ?? annotation(node, "x-category") ?? key
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
    @State private var addingTool = false
    @State private var newToolName = ""

    /// Pages whose entries are listed under them in the sidebar.
    private static let collections: Set<String> = ["repositories", "projects", "workflows", "tools"]

    var body: some View {
        // A fixed two-pane window: a settings window has no use for the
        // collapsible sidebar (and its toolbar toggle) of NavigationSplitView.
        HStack(spacing: 0) {
            List(selection: $selection) {
                row("general", "General", "gearshape")
                row("check", "Check", "checkmark.seal")
                Section {
                    ForEach(model.layout.settings) { page in
                        if Self.collections.contains(page.id) {
                            collectionRows(page)
                        } else {
                            row(page.id, model.title(of: page), icon(for: page.id))
                        }
                    }
                } header: { header("Settings") }
                Section {
                    ForEach(model.layout.plugins) { row($0.id, model.title(of: $0), icon(for: $0.id)) }
                } header: { header("Plugins") }
            }
            .listStyle(.sidebar)
            .frame(width: 220)
            Divider()
            Form {
                if let error = model.error {
                    Section { Text(error).foregroundStyle(.red).textSelection(.enabled) }
                }
                detail
            }
            .formStyle(.grouped)
        }
        .frame(minWidth: 760, minHeight: 520)
        .task { await model.reload() }
    }

    private func row(_ id: String, _ title: String, _ symbol: String) -> some View {
        Label(title, systemImage: symbol).tag(id)
    }

    /// A collection, a ＋ at its trailing edge, and its entries indented under
    /// it — always shown, so nothing animates in from above.
    @ViewBuilder private func collectionRows(_ page: SettingsPage) -> some View {
        HStack(spacing: 6) {
            Label(model.title(of: page), systemImage: icon(for: page.id))
            Spacer()
            Button { add(to: page.id) } label: { Image(systemName: "plus.circle") }
                .buttonStyle(.borderless)
                .help("Add")
                .popover(isPresented: page.id == "tools" ? $addingTool : .constant(false)) {
                    HStack {
                        TextField("name, e.g. claude-fast", text: $newToolName)
                            .frame(width: 200)
                            .onSubmit { addTool() }
                        Button("Add") { addTool() }.disabled(newToolName.isEmpty)
                    }
                    .padding()
                }
        }
        .tag(page.id)
        ForEach(model.children(of: page.id), id: \.id) { child in
            Text(child.title).lineLimit(1).padding(.leading, 28).tag(child.id)
        }
    }

    private func add(to key: String) {
        if key == "tools" {
            addingTool = true
            return
        }
        Task {
            if let id = await model.addElement(to: key) { selection = id }
        }
    }

    private func addTool() {
        let name = newToolName.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty else { return }
        newToolName = ""
        addingTool = false
        Task {
            if let id = await model.addEntry(to: "tools", named: name) { selection = id }
        }
    }

    /// Section headers read as headers, not as one more item.
    private func header(_ text: String) -> some View {
        Text(text.uppercased())
            .font(.caption.weight(.semibold))
            .foregroundStyle(.secondary)
    }

    private func icon(for id: String) -> String {
        switch id {
        case "repositories": return "folder"
        case "projects": return "rectangle.stack"
        case "workflows": return "arrow.triangle.branch"
        case "tools": return "hammer"
        case "llm": return "sparkles"
        case "log": return "doc.text"
        case "hooks": return "link"
        case "github": return "chevron.left.forwardslash.chevron.right"
        case "notion": return "doc.richtext"
        case "slack": return "number"
        case "discord": return "bubble.left.and.bubble.right"
        case "herdr", "orca": return "terminal"
        case "macos": return "bell"
        default: return "puzzlepiece.extension"
        }
    }

    @ViewBuilder private var detail: some View {
        let layout = model.layout
        switch selection {
        case "check":
            CheckView(model: model)
        case "general", nil:
            GeneralSettings(app: app, configPath: model.configPath)
            fields(layout.general.keys)
        case let id? where id.contains("/"):
            element(id)
        case let id?:
            if let page = (layout.settings + layout.plugins).first(where: { $0.id == id }) {
                if page.id == "tools" {
                    Section {
                        Text("Built in: claude, codex and opencode. Add others (e.g. claude-fast) with ＋ in the sidebar to pick them in repositories and workflows; an entry named like a built-in overrides it.")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    fields(["default_tool"])
                } else if Self.collections.contains(page.id) {
                    Section {
                        Text("Add one with ＋ in the sidebar, then select it to edit.")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                } else if layout.plugins.contains(page) {
                    PluginSection(model: model, plugin: page.id)
                    fields(page.keys)
                } else {
                    fields(page.keys)
                }
            }
        }
    }

    @ViewBuilder private func fields(_ keys: [String]) -> some View {
        ForEach(keys, id: \.self) { key in
            if let prop = model.property(key) {
                FieldEditor(model: model, segments: [key], schema: prop.schema, required: prop.required, depth: 0)
            }
        }
    }

    /// One entry of a collection (`repositories/2`, `tools/claude-fast`).
    @ViewBuilder private func element(_ id: String) -> some View {
        let parts = id.split(separator: "/", maxSplits: 1).map(String.init)
        if parts.count == 2, let schema = model.elementSchema(parts[0]) {
            FieldEditor(model: model, segments: parts, schema: schema, required: true, depth: 0)
            Section {
                Button("Delete", role: .destructive) {
                    Task {
                        await model.unset(parts)
                        selection = parts[0]
                    }
                }
            }
        }
    }
}

/// A plugin page's first section: whether it runs, and the roster settings
/// every plugin shares (`[plugins.<name>]`). `kind` comes from the manifest and
/// is not offered.
struct PluginSection: View {
    @ObservedObject var model: SettingsModel
    let plugin: String

    var body: some View {
        Section("Plugin") {
            Toggle("Enabled", isOn: Binding(
                get: { model.isEnabled(plugin) },
                set: { on in Task { await model.setEnabled(plugin, on) } }))
            if model.config["plugins"]?[plugin] != nil, let entry = model.rosterEntrySchema,
                case .object(let props) = fieldKind(entry)
            {
                FieldRows(model: model, segments: ["plugins", plugin],
                          properties: props.filter { !["kind", "enabled"].contains($0.key) })
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
        Section {
            LabeledContent("Config file", value: configPath)
            TextField("totsuka", text: $totsukaPath, prompt: Text("found on PATH"))
            TextField("PATH", text: $pathOverride, prompt: Text("the login shell's"))
            Toggle("Open at login", isOn: Binding(
                get: { app.launchesAtLogin }, set: { app.launchesAtLogin = $0 }))
        } header: {
            Text("App")
        } footer: {
            Text("Path changes take effect after restarting the app.")
                .font(.caption).foregroundStyle(.secondary)
        }
    }
}

struct CheckView: View {
    @ObservedObject var model: SettingsModel

    var body: some View {
        Section {
            Text("Starting requires `totsuka config validate` to pass.")
            Button("Check now") { Task { await model.validate() } }
            if let result = model.validation {
                Label(result.ok ? "Passes" : "Does not pass",
                      systemImage: result.ok ? "checkmark.circle" : "xmark.circle")
                    .foregroundStyle(result.ok ? .green : .red)
                Text(result.output).font(.system(.callout, design: .monospaced)).textSelection(.enabled)
            }
        }
    }
}

/// The rows of a table: what to fill in first, then the optional settings that
/// already have a default under a collapsed "Advanced".
struct FieldRows: View {
    @ObservedObject var model: SettingsModel
    let segments: [String]
    let properties: [Property]
    var depth = 1

    var body: some View {
        let advanced = properties.filter(isAdvanced)
        ForEach(properties.filter { !isAdvanced($0) }) { prop in
            AnyView(FieldEditor(model: model, segments: segments + [prop.key],
                                schema: prop.schema, required: prop.required, depth: depth))
        }
        if !advanced.isEmpty {
            Collapsible(expanded: false) { Text("Advanced") } content: {
                ForEach(advanced) { prop in
                    AnyView(FieldEditor(model: model, segments: segments + [prop.key],
                                        schema: prop.schema, required: prop.required, depth: depth))
                }
            }
        }
    }
}

/// A collapsible run of form rows. Not `DisclosureGroup`: in a grouped `Form`
/// that only takes clicks on its chevron and packs the rows it reveals without
/// the form's row spacing. Here the whole header row toggles, and the revealed
/// rows are ordinary rows of the form, indented under the header.
struct Collapsible<Label: View, Content: View>: View {
    @State private var expanded: Bool
    private let label: Label
    private let content: Content

    init(expanded: Bool = true, @ViewBuilder label: () -> Label, @ViewBuilder content: () -> Content) {
        _expanded = State(initialValue: expanded)
        self.label = label()
        self.content = content()
    }

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "chevron.right")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .rotationEffect(.degrees(expanded ? 90 : 0))
            label
            Spacer(minLength: 0)
        }
        .contentShape(Rectangle())
        .onTapGesture { withAnimation(.easeInOut(duration: 0.15)) { expanded.toggle() } }
        .accessibilityAddTraits(.isButton)
        .accessibilityValue(expanded ? "expanded" : "collapsed")
        if expanded {
            Group { content }.padding(.leading, 18)
        }
    }
}

/// One schema node as form rows: a table becomes a section (top level) or a
/// group (nested), everything else one aligned label + control row.
struct FieldEditor: View {
    @ObservedObject var model: SettingsModel
    let segments: [String]
    let schema: JSONValue
    let required: Bool
    var depth = 1
    @State private var text = ""
    @State private var newKey = ""
    @State private var showingHelp = false

    private var node: JSONValue { nonNull(schema) }
    private var value: JSONValue? { model.value(at: segments) }
    private var title: String {
        annotation(node, "x-title") ?? annotation(node, "x-category") ?? entryTitle
    }
    /// An array element or a map entry: its name.
    private var entryTitle: String {
        value?["name"]?.string.flatMap { $0.isEmpty ? nil : $0 } ?? segments.last ?? ""
    }

    var body: some View {
        if let (ref, multiple) = reference(for: segments) {
            referencePicker(ref, multiple: multiple)
        } else {
            editor
        }
    }

    /// A field naming something configured elsewhere: picked from the list,
    /// not typed. A current value the list does not have is still shown, so
    /// a typo in the file is visible rather than silently replaced.
    @ViewBuilder private func referencePicker(_ ref: Reference, multiple: Bool) -> some View {
        let options = referenceOptions(ref, config: model.config)
        if multiple {
            let selected = (value?.array ?? []).compactMap(\.string)
            LabeledContent {
                Menu(selected.isEmpty ? "(none)" : selected.joined(separator: ", ")) {
                    ForEach(Array(Set(options + selected)).sorted(), id: \.self) { name in
                        Toggle(name, isOn: Binding(
                            get: { selected.contains(name) },
                            set: { on in
                                let next = on ? selected + [name] : selected.filter { $0 != name }
                                Task { await model.set(segments, .array(next.map(JSONValue.string))) }
                            }))
                    }
                }
                .fixedSize()
            } label: { label }
        } else {
            let current = value?.string ?? ""
            Picker(selection: Binding(
                get: { current },
                set: { new in Task {
                    if new.isEmpty { await model.unset(segments) } else { await model.set(segments, .string(new)) }
                } })
            ) {
                if !required { Text(defaultLabel).tag("") }
                ForEach(options, id: \.self) { Text($0).tag($0) }
                if !current.isEmpty, !options.contains(current) {
                    Text(current + " (not configured)").tag(current)
                }
            } label: { label }
        }
    }

    @ViewBuilder private var editor: some View {
        switch fieldKind(schema) {
        case .bool:
            Toggle(isOn: Binding(
                get: { value?.bool ?? node["default"]?.bool ?? false },
                set: { new in Task { await model.set(segments, .bool(new)) } })
            ) { label }
        case .string where isMultiline(segments, value: value):
            multiline
        case .integer, .number, .string:
            TextField(text: $text, prompt: placeholder(schema).map { Text($0) }) { label }
                .onAppear { text = scalarText(value) }
                .onChange(of: value) { _, new in text = scalarText(new) }
                .onSubmit { Task { await commitScalar() } }
        case .secret:
            SecureField(text: $text, prompt: Text(secretPlaceholder)) { label }
                .onSubmit {
                    let entered = text
                    text = ""
                    Task { await model.saveSecret(segments, entered) }
                }
        case .choice(let choices):
            Picker(selection: Binding(
                get: { value?.string ?? "" },
                set: { new in Task {
                    if new.isEmpty { await model.unset(segments) } else { await model.set(segments, .string(new)) }
                } })
            ) {
                if !required { Text(defaultLabel).tag("") }
                ForEach(choices, id: \.self) { Text($0).tag($0) }
            } label: { label }
        case .choiceOrObject(let choices, let object):
            // A name, or a table instead (cleanup: `{ retention_days = N }`).
            let custom = value?.object != nil
            Picker(selection: Binding(
                get: { custom ? Self.customTag : (value?.string ?? "") },
                set: { new in Task {
                    switch new {
                    case "": await model.unset(segments)
                    case Self.customTag: await model.set(segments, newValue(for: object))
                    default: await model.set(segments, .string(new))
                    }
                } })
            ) {
                if !required { Text(defaultLabel).tag("") }
                ForEach(choices, id: \.self) { Text($0).tag($0) }
                Text("Number of days…").tag(Self.customTag)
            } label: { label }
            if custom, case .object(let props) = fieldKind(object) {
                FieldRows(model: model, segments: segments, properties: props, depth: depth + 1)
            }
        case .stringList:
            TextField(text: $text, prompt: Text("comma separated")) { label }
                .onAppear { text = listText(value) }
                .onChange(of: value) { _, new in text = listText(new) }
                .onSubmit { Task { await commitList() } }
        case .object(let props):
            if depth == 0 {
                Section { FieldRows(model: model, segments: segments, properties: props) } header: { label }
            } else {
                Collapsible { label } content: {
                    FieldRows(model: model, segments: segments, properties: props, depth: depth + 1)
                }
            }
        case .list(let item):
            group {
                ForEach(Array((value?.array ?? []).enumerated()), id: \.offset) { index, element in
                    Collapsible(expanded: false) {
                        Text(element["name"]?.string ?? "#\(index + 1)")
                    } content: {
                        AnyView(FieldEditor(model: model, segments: segments + [String(index)],
                                            schema: item, required: true, depth: depth + 1))
                        Button("Remove", role: .destructive) {
                            Task { await model.unset(segments + [String(index)]) }
                        }
                    }
                }
                Button("Add") {
                    Task { await model.set(segments + ["-"], newValue(for: item)) }
                }
            }
        case .map(let valueSchema):
            group {
                ForEach((value?.object ?? [:]).keys.sorted(), id: \.self) { key in
                    Collapsible(expanded: false) { Text(key) } content: {
                        AnyView(FieldEditor(model: model, segments: segments + [key],
                                            schema: valueSchema, required: true, depth: depth + 1))
                        Button("Remove", role: .destructive) {
                            Task { await model.unset(segments + [key]) }
                        }
                    }
                }
                HStack {
                    TextField("name", text: $newKey)
                    Button("Add") {
                        let key = newKey
                        newKey = ""
                        Task { await model.set(segments + [key], newValue(for: valueSchema)) }
                    }
                    .disabled(newKey.isEmpty)
                }
            }
        case .raw(let reason):
            group {
                if let reason { Text(reason).font(.caption).foregroundStyle(.secondary) }
                TextEditor(text: $text)
                    .font(.system(.body, design: .monospaced))
                    .frame(minHeight: 80)
                    .onAppear { text = (value ?? .object([:])).prettyText }
                Button("Save") {
                    if let parsed = JSONValue.parse(text) {
                        Task { await model.set(segments, parsed) }
                    } else {
                        model.error = "Not valid JSON"
                    }
                }
            }
        }
    }

    /// A section at the top level, a disclosure group inside one.
    @ViewBuilder private func group<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        let rows = content()
        if depth == 0 {
            Section { rows } header: { label }
        } else {
            Collapsible { label } content: { rows }
        }
    }

    /// A text long enough to wrap: an editor several lines tall, saved with a
    /// button (Return is a newline here).
    @ViewBuilder private var multiline: some View {
        VStack(alignment: .leading, spacing: 6) {
            label
            TextEditor(text: $text)
                .font(.body)
                .frame(minHeight: 90)
                .onAppear { text = scalarText(value) }
                .onChange(of: value) { _, new in text = scalarText(new) }
            HStack {
                if text.isEmpty, let hint = placeholder(schema) {
                    Text(hint).font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Button("Save") { Task { await commitScalar() } }
                    .disabled(text == scalarText(value))
            }
        }
    }

    /// The title, a required mark, and the help popover (`x-help`).
    private var label: some View {
        HStack(spacing: 4) {
            Text(title + (required && depth > 0 ? " *" : ""))
            if let help = annotation(node, "x-help") {
                Button { showingHelp.toggle() } label: { Image(systemName: "questionmark.circle") }
                    .buttonStyle(.borderless)
                    .popover(isPresented: $showingHelp) {
                        // Wrapped, not truncated: help is a paragraph.
                        Text(help)
                            .font(.callout)
                            .fixedSize(horizontal: false, vertical: true)
                            .frame(width: 300, alignment: .leading)
                            .padding()
                    }
            }
        }
    }

    private static let customTag = "\u{1}custom"

    /// The "use the default" option, naming the default when it is known.
    private var defaultLabel: String {
        placeholder(schema).map { "(default: \($0))" } ?? "(default)"
    }

    private var secretPlaceholder: String {
        switch value?.string {
        case let ref? where ref.hasPrefix("secret:"): return "saved — type to replace"
        // `op://` and the like are refused under `--secrets-stdin`.
        case .some: return "needs input (the current reference is not used by the app)"
        case nil: return "not set"
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
                model.error = "Not a whole number: " + trimmed
                return
            }
            await model.set(segments, .int(i))
        case .number:
            guard let d = Double(trimmed) else {
                model.error = "Not a number: " + trimmed
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
