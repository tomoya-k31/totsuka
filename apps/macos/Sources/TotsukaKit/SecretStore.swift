import Foundation
import Security

/// The app's secrets: one Keychain item holding a JSON map
/// `{"<secret name>": "<value>"}` (ADR-0109 §5). `run` receives the whole map
/// on stdin and resolves `secret:<name>` from it, so config.toml only ever
/// holds names.
///
/// One item rather than one per secret: with an ad-hoc-signed app every build
/// is a new identity to the Keychain, and each item asks again — one item
/// means one prompt per update (measured, ADR-0109).
public struct SecretStore: Sendable {
    public let service: String
    public let account: String

    public init(service: String = "io.github.tomoya-k31.totsuka", account: String = "secrets") {
        self.service = service
        self.account = account
    }

    private var query: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    /// The map, empty when there is no item yet.
    public func load() throws -> [String: String] {
        var q = query
        q[kSecReturnData as String] = true
        var out: AnyObject?
        let status = SecItemCopyMatching(q as CFDictionary, &out)
        if status == errSecItemNotFound { return [:] }
        guard status == errSecSuccess, let data = out as? Data else {
            throw KeychainError(status: status)
        }
        // A map that does not decode is an error, not an empty map: saving
        // over it would erase every stored secret.
        return try JSONDecoder().decode([String: String].self, from: data)
    }

    /// Replace the map.
    public func save(_ secrets: [String: String]) throws {
        let data = try JSONEncoder().encode(secrets)
        let status = SecItemUpdate(
            query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecItemNotFound {
            var add = query
            add[kSecValueData as String] = data
            let added = SecItemAdd(add as CFDictionary, nil)
            guard added == errSecSuccess else { throw KeychainError(status: added) }
        } else if status != errSecSuccess {
            throw KeychainError(status: status)
        }
    }
}

public struct KeychainError: Error, CustomStringConvertible, Sendable {
    public let status: OSStatus
    public var description: String {
        (SecCopyErrorMessageString(status, nil) as String?) ?? "Keychain error \(status)"
    }
}
