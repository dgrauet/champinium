// swift-tools-version:5.9
// Champinium — front macOS natif (SwiftUI). Présentation uniquement.
//
// Consomme le noyau Rust via les bindings UniFFI générés :
//   - ChampiniumCoreFFI.xcframework : la lib native + le module C (généré par
//     `just macos-prepare`, gitignoré) ;
//   - Sources/ChampiniumCore/ChampiniumCore.swift : le wrapper Swift généré
//     (copié par `just macos-prepare`, gitignoré).
// Lecture média : AVPlayer / AVFoundation.
import PackageDescription

let package = Package(
    name: "Champinium",
    platforms: [.macOS(.v13)],
    targets: [
        .binaryTarget(
            name: "ChampiniumCoreFFI",
            path: "Frameworks/ChampiniumCoreFFI.xcframework"
        ),
        .target(
            name: "ChampiniumCore",
            dependencies: ["ChampiniumCoreFFI"],
            path: "Sources/ChampiniumCore"
        ),
        .executableTarget(
            name: "Champinium",
            dependencies: ["ChampiniumCore"],
            path: "Sources/Champinium"
        ),
    ]
)
