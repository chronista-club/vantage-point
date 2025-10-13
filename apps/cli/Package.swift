// swift-tools-version: 6.0
// The swift-tools-version declares the minimum version of Swift required to build this package.

import PackageDescription

let package = Package(
    name: "VantagePointCLI",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(
            name: "vantage",
            targets: ["VantagePointCLI"]
        )
    ],
    dependencies: [
        // コマンドライン引数パーサー
        .package(url: "https://github.com/apple/swift-argument-parser", from: "1.3.0"),
        
        // 親プロジェクトのパッケージ
        .package(name: "VANTAGE", path: "../"),
        
        // ロギング
        .package(url: "https://github.com/apple/swift-log", from: "1.5.0"),
    ],
    targets: [
        // CLI実行可能ターゲット
        .executableTarget(
            name: "VantagePointCLI",
            dependencies: [
                .product(name: "ArgumentParser", package: "swift-argument-parser"),
                .product(name: "ClaudeIntegration", package: "VANTAGE"),
                .product(name: "Logging", package: "swift-log"),
                "VantageCore"
            ]
        ),
        
        // コアライブラリターゲット
        .target(
            name: "VantageCore",
            dependencies: [
                .product(name: "ClaudeIntegration", package: "VANTAGE"),
                .product(name: "Logging", package: "swift-log")
            ]
        ),
        
        // テストターゲット
        .testTarget(
            name: "VantagePointCLITests",
            dependencies: ["VantagePointCLI"]
        ),
        .testTarget(
            name: "VantageCoreTests",
            dependencies: ["VantageCore"]
        ),
    ]
)