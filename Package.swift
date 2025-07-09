// swift-tools-version: 6.0
// The swift-tools-version declares the minimum version of Swift required to build this package.

import PackageDescription

let package = Package(
    name: "ClaudeAPI",
    platforms: [
        .macOS(.v14),
        .iOS(.v17),
        .visionOS(.v2)
    ],
    products: [
        .library(
            name: "ClaudeAPI",
            targets: ["ClaudeAPI"]
        ),
    ],
    dependencies: [],
    targets: [
        .target(
            name: "ClaudeAPI",
            dependencies: [],
            path: "Sources/ClaudeAPI"
        ),
        .testTarget(
            name: "ClaudeAPITests",
            dependencies: ["ClaudeAPI"],
            path: "Tests/ClaudeAPITests"
        ),
    ]
)