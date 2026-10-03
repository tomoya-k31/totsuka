import Foundation

/// The form of one `[[projects]]` / `[[workflows]]` entry: the core's keys plus
/// the keys of the plugins the entry uses, which `config schema` attaches to
/// the item schema by plugin name (`x-by-source` / `x-by-agent`, ADR-0109 §5).
///
/// Only the chosen source's (and agent's) keys are shown, rather than every
/// plugin's with the others disabled: the sets barely overlap, and a key left
/// from another source is not "unavailable" but unused — so it is listed in
/// `unused` with a way to remove it.
public struct EntryForm: Equatable, Sendable {
    /// An object schema: the core's properties with the plugins' laid over
    /// them (a plugin's `trigger` replaces the core's untyped one).
    public let schema: JSONValue
    /// Keys on the entry that nothing reads with its current source and
    /// agent. Empty unless every plugin the entry uses described its keys —
    /// otherwise an extra key may well be one of theirs.
    public let unused: [String]
    /// The same for the entry's `trigger` table.
    public let unusedTrigger: [String]
}

/// The form for `element`, an entry of the `collection` array (`projects` or
/// `workflows`) whose item schema is `item`.
public func entryForm(
    collection: String, item: JSONValue, element: JSONValue, config: JSONValue
) -> EntryForm {
    var properties = item["properties"]?.object ?? [:]
    var required = Set(item["required"]?.array?.compactMap(\.string) ?? [])
    var described = true

    func merge(_ keyword: String, _ plugin: String?) {
        guard let plugin, !plugin.isEmpty,
            let extra = item[keyword]?[plugin], let props = extra["properties"]?.object
        else {
            described = false
            return
        }
        for (key, value) in props { properties[key] = value }
        required.formUnion(extra["required"]?.array?.compactMap(\.string) ?? [])
    }

    switch collection {
    case "projects":
        merge("x-by-source", element["source"]?.string)
    case "workflows":
        merge("x-by-source", workflowSource(element, config: config))
        merge("x-by-agent", element["agent"]?.string)
    default:
        described = false
    }

    var schema = item.object ?? [:]
    schema["properties"] = .object(properties)
    schema["required"] = .array(required.sorted().map(JSONValue.string))
    schema["x-by-source"] = nil
    schema["x-by-agent"] = nil

    var unused: [String] = []
    var unusedTrigger: [String] = []
    if described {
        let keys = (element.object ?? [:]).keys
        unused = keys.filter { properties[$0] == nil }.sorted()
        if let known = properties["trigger"].map(nonNull)?["properties"]?.object {
            let written = (element["trigger"]?.object ?? [:]).keys
            unusedTrigger = written.filter { known[$0] == nil }.sorted()
        }
    }
    return EntryForm(schema: .object(schema), unused: unused, unusedTrigger: unusedTrigger)
}

/// The task source a workflow takes tasks from: the `source` of the first
/// project it names that is configured. A workflow's projects must all share
/// one source (`config validate` refuses otherwise), so the first is enough.
public func workflowSource(_ workflow: JSONValue, config: JSONValue) -> String? {
    let projects = config["projects"]?.array ?? []
    for name in workflow["projects"]?.array?.compactMap(\.string) ?? [] {
        if let source = projects.first(where: { $0["name"]?.string == name })?["source"]?.string,
            !source.isEmpty
        {
            return source
        }
    }
    return nil
}
