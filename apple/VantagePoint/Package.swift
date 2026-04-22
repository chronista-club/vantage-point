// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "VantagePoint",
    platforms: [
        .macOS(.v26)
    ],
    products: [
        .executable(name: "VantagePoint", targets: ["VantagePoint"])
    ],
    dependencies: [
        // CreoUI design system — local path dependency
        // (creo-ui repo は packages/swift サブディレクトリに SPM manifest を持つ)
        .package(path: "../../../creo-ui/packages/swift"),
    ],
    targets: [
        .executableTarget(
            name: "VantagePoint",
            dependencies: [
                "VPBridge",
                // package 名は SPM が path dep の directory 名 ("swift") として解釈する
                // (creo-ui/packages/swift/Package.swift の name: "CreoUI" ではなく)
                .product(name: "CreoUI", package: "swift"),
            ],
            path: "Sources",
            linkerSettings: [
                // libvp_bridge.a をリンク（cargo build --release で生成）
                .unsafeFlags([
                    "-L../../target/release",
                    "-lvp_bridge",
                ]),
            ]
        ),
        // vp-bridge の C ヘッダーを Swift から使えるようにする
        .systemLibrary(
            name: "VPBridge",
            path: "VPBridge"
        )
    ]
)
