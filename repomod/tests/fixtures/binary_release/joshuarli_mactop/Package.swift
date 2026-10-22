// swift-tools-version: 6.4
import PackageDescription

let swift6Settings: [SwiftSetting] = [
    .swiftLanguageMode(.v6),
    .enableUpcomingFeature("NonisolatedNonsendingByDefault"),
]

let package = Package(
    name: "mactop",
    platforms: [.macOS("27.0")],
    products: [
        .library(name: "mactopCore", targets: ["mactopCore"]),
        .executable(name: "mactop", targets: ["mactop"]),
        .executable(name: "mactopBench", targets: ["mactopBench"]),
    ],
    targets: [
        .target(
            name: "mactopPlatform",
            path: "Sources/mactop/Platform",
            swiftSettings: [.swiftLanguageMode(.v6)],
            linkerSettings: [
                .linkedFramework("IOKit"),
                .linkedFramework("SystemConfiguration"),
            ]
        ),
        .target(
            name: "mactopCore",
            dependencies: ["mactopPlatform"],
            path: "Sources/mactop/Core",
            swiftSettings: swift6Settings,
        ),
        .executableTarget(
            name: "mactop",
            dependencies: ["mactopCore"],
            path: "Sources/mactop/UI",
            swiftSettings: [
                .enableUpcomingFeature("NonisolatedNonsendingByDefault"),
                .defaultIsolation(MainActor.self),
                .swiftLanguageMode(.v6),
            ],
        ),
        .executableTarget(
            name: "mactopBench",
            dependencies: ["mactopCore"],
            path: "Sources/mactopBench",
            swiftSettings: swift6Settings,
            linkerSettings: [
                .linkedFramework("IOKit"),
                .linkedFramework("SystemConfiguration"),
            ]
        ),
        .testTarget(
            name: "mactopTests",
            dependencies: ["mactopCore", "mactopPlatform"],
            swiftSettings: swift6Settings
        ),
    ]
)
