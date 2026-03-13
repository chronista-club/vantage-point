// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "VantagePoint",
    platforms: [
        .macOS(.v13)
    ],
    products: [
        .executable(name: "VantagePoint", targets: ["VantagePoint"])
    ],
    targets: [
        .executableTarget(
            name: "VantagePoint",
            dependencies: ["VPBridge"],
            path: "Sources",
            linkerSettings: [
                // libvp_bridge.a をリンク（cargo build --release で生成）
                .unsafeFlags([
                    "-L../../vantage-point/target/release",
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
