// swift-tools-version: 6.0
// The menu bar app (ADR-0109). `TotsukaKit` holds everything testable without
// a window — process supervision, the CLI contract, the notification filter,
// the Keychain map — and `swift test` runs on Command Line Tools alone. The
// `Totsuka` executable is the SwiftUI app; CI builds the shipped `.app` from
// `project.yml` (XcodeGen), which adds the asset catalog this package cannot
// compile without Xcode.
import PackageDescription

let package = Package(
    name: "TotsukaKit",
    platforms: [.macOS(.v15)],
    products: [.library(name: "TotsukaKit", targets: ["TotsukaKit"])],
    targets: [
        .target(name: "TotsukaKit"),
        // Not named `Totsuka`: the Xcode project's app target is, and a package
        // scheme of the same name would win `xcodebuild -scheme Totsuka`,
        // building a bare binary instead of the `.app`.
        .executableTarget(name: "TotsukaApp", dependencies: ["TotsukaKit"], path: "Sources/Totsuka"),
        .testTarget(name: "TotsukaKitTests", dependencies: ["TotsukaKit"]),
    ],
    // Swift 5 mode: `Process` / `FileHandle` callbacks are not `Sendable`, and
    // the strict checks would push every one of them behind an actor for no
    // behaviour this app has.
    swiftLanguageModes: [.v5]
)
