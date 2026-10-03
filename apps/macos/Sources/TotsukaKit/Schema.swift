import Foundation

/// How the settings window edits one schema node (`totsuka config schema`).
public indirect enum FieldKind: Equatable, Sendable {
    case bool
    case integer
    case number
    case string
    /// `x-secret`: a value kept in the Keychain, written to config as
    /// `secret:<name>`.
    case secret
    case choice([String])
    /// One of `choices`, or a table of the given schema instead (a cleanup
    /// policy: a name, or `{ retention_days = N }`).
    case choiceOrObject([String], object: JSONValue)
    case stringList
    /// A table with known keys.
    case object([Property])
    /// An array of tables (`[[repositories]]`).
    case list(item: JSONValue)
    /// A table of named entries (`[tools.<name>]`, `[plugins.<name>]`).
    case map(value: JSONValue)
    /// No form: edited as JSON. `reason` is the CLI's `x-schema-error`.
    case raw(reason: String?)
}

/// One named property of an object node.
public struct Property: Equatable, Sendable, Identifiable {
    public let key: String
    public let schema: JSONValue
    public let required: Bool
    public var id: String { key }
}

/// A text annotation (`x-title`, `x-help`, `x-category`) on `schema`.
public func annotation(_ schema: JSONValue, _ keyword: String) -> String? {
    schema[keyword]?.string
}

/// The schema without its `null` alternative: `Option<T>` arrives as
/// `"type": ["string", "null"]` or `anyOf: [T, {"type": "null"}]`.
public func nonNull(_ schema: JSONValue) -> JSONValue {
    guard var object = schema.object else { return schema }
    if let types = object["type"]?.array {
        let rest = types.filter { $0 != .string("null") }
        if rest.count == 1 { object["type"] = rest[0] }
        return .object(object)
    }
    if let any = object["anyOf"]?.array {
        let rest = any.filter { $0["type"] != .string("null") }
        if rest.count == 1, var inner = rest[0].object {
            // Keep the outer node's annotations (`x-title` sits there).
            for (key, value) in object where key.hasPrefix("x-") { inner[key] = value }
            return .object(inner)
        }
    }
    return schema
}

/// How to edit `schema`.
public func fieldKind(_ schema: JSONValue) -> FieldKind {
    let s = nonNull(schema)
    if s["x-raw"]?.bool == true { return .raw(reason: s["x-schema-error"]?.string) }
    if s["x-secret"]?.bool == true { return .secret }
    if let choices = enumChoices(s) { return .choice(choices) }
    if acceptsOneOrMany(s) { return .stringList }
    if let alternatives = s["anyOf"]?.array, alternatives.count == 2,
        let choices = alternatives.lazy.compactMap(enumChoices).first,
        let object = alternatives.first(where: { $0["properties"] != nil })
    {
        return .choiceOrObject(choices, object: object)
    }
    switch s["type"]?.string {
    case "boolean": return .bool
    case "integer": return .integer
    case "number": return .number
    case "string": return .string
    case "array":
        let items = nonNull(s["items"] ?? .null)
        if items["type"]?.string == "string" { return .stringList }
        if items["properties"] != nil { return .list(item: items) }
        return .raw(reason: nil)
    case "object":
        if let props = s["properties"]?.object, !props.isEmpty {
            let required = Set(s["required"]?.array?.compactMap(\.string) ?? [])
            let ordered = props.keys.sorted { a, b in
                // Required keys first, then alphabetical (the schema's own
                // order does not survive JSON decoding).
                (required.contains(a) ? 0 : 1, a) < (required.contains(b) ? 0 : 1, b)
            }
            return .object(
                ordered.map { Property(key: $0, schema: props[$0]!, required: required.contains($0)) })
        }
        if let value = s["additionalProperties"], value.object != nil {
            let kind = fieldKind(value)
            if case .raw = kind { return .raw(reason: nil) }
            return .map(value: value)
        }
        return .raw(reason: nil)
    default:
        return .raw(reason: nil)
    }
}

/// Whether `schema` takes one string or a list of them (`anyOf` of a string
/// and an array of strings) — a trigger's `label` or `assignee`. Edited as a
/// list; a single entry is written as a plain string, since the reader treats
/// the two alike and some values (`assignee = "@any"`) are only valid alone.
public func acceptsOneOrMany(_ schema: JSONValue) -> Bool {
    guard let alternatives = nonNull(schema)["anyOf"]?.array, alternatives.count == 2 else {
        return false
    }
    let string = alternatives.contains { $0["type"]?.string == "string" }
    let list = alternatives.contains {
        $0["type"]?.string == "array" && $0["items"]?["type"]?.string == "string"
    }
    return string && list
}

/// The allowed strings of an enum node (`enum`, or `oneOf` of `const`s).
public func enumChoices(_ schema: JSONValue) -> [String]? {
    if let values = schema["enum"]?.array {
        let strings = values.compactMap(\.string)
        return strings.count == values.count ? strings : nil
    }
    if let alternatives = schema["oneOf"]?.array {
        let consts = alternatives.compactMap { $0["const"]?.string }
        return consts.count == alternatives.count ? consts : nil
    }
    return nil
}

/// A new value for `schema` that loads: the required keys filled with empty
/// values. Used when adding an array element or a map entry — `config set`
/// refuses a write that leaves the file unloadable.
public func newValue(for schema: JSONValue) -> JSONValue {
    switch fieldKind(schema) {
    case .bool: return .bool(false)
    case .integer: return .int(0)
    case .number: return .double(0)
    case .string, .secret: return .string("")
    case .choice(let choices), .choiceOrObject(let choices, _): return .string(choices.first ?? "")
    case .stringList, .list: return .array([])
    case .map, .raw: return .object([:])
    case .object(let properties):
        var object: [String: JSONValue] = [:]
        for p in properties where p.required { object[p.key] = newValue(for: p.schema) }
        return .object(object)
    }
}

/// The text an empty field shows: `x-placeholder` (a default the code
/// decides), else the schema's own `default` (one serde knows).
public func placeholder(_ schema: JSONValue) -> String? {
    let node = nonNull(schema)
    if let text = node["x-placeholder"]?.string ?? schema["x-placeholder"]?.string { return text }
    switch node["default"] ?? schema["default"] {
    case .string(let s)?: return s
    case .int(let i)?: return String(i)
    case .double(let d)?: return String(d)
    default: return nil
    }
}

/// One row of `totsuka plugin list --json`.
public struct PluginInfo: Decodable, Equatable, Sendable {
    public let name: String
    public let kind: String
    public let enabled: Bool

    public static func decodeList(_ data: Data) throws -> [PluginInfo] {
        try JSONDecoder().decode([PluginInfo].self, from: data)
    }
}

/// Whether a text field is long enough to deserve a multi-line editor:
/// instruction and prompt texts by their key, and any value that already
/// spans lines.
public func isMultiline(_ segments: [String], value: JSONValue?) -> Bool {
    if value?.string?.contains("\n") == true { return true }
    let key = segments.last ?? ""
    return segments.contains("prompts") || key.hasSuffix("_instructions")
        || ["rubric", "initial_prompt", "reply_style", "summary"].contains(key)
}

/// Whether a property belongs under "Advanced": optional, with a default the
/// field already shows. What a person must fill in, or may want to without a
/// default to fall back on, stays visible.
public func isAdvanced(_ property: Property) -> Bool {
    !property.required && placeholder(property.schema) != nil
        || !property.required && nonNull(property.schema)["default"]?.bool != nil
}
