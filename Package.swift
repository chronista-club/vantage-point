// swift-tools-version: 6.0
// The swift-tools-version declares the minimum version of Swift required to build this package.

import PackageDescription

let package = Package(
    name: "ClaudeIntegration",
    platforms: [
        .macOS(.v14),
        .iOS(.v17),
        .visionOS(.v2),
    ],
    products: [
        .library(
            name: "ClaudeIntegration",
            targets: ["ClaudeIntegration"]
        )
    ],
    dependencies: [],
    targets: [
        .target(
            name: "ClaudeIntegration",
            dependencies: [],
            path: "Sources/ClaudeIntegration"
        ),
        .testTarget(
            name: "ClaudeIntegrationTests",
            dependencies: ["ClaudeIntegration"],
            path: "Tests/ClaudeIntegrationTests"
        ),
    ]
)
