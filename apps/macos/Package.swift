// swift-tools-version:5.9
// Champinium — front macOS natif (SwiftUI). SQUELETTE.
//
// Présentation UNIQUEMENT. Toute la logique vit dans champinium-core (Rust),
// consommé via les bindings UniFFI générés (XCFramework "ChampiniumCoreFFI" +
// fichier Swift "ChampiniumCore"), produits par `just gen-swift`.
// Lecture média cible : AVPlayer / AVFoundation.
import PackageDescription

let package = Package(
    name: "Champinium",
    platforms: [.macOS(.v13)],
    targets: [
        // Cible applicative SwiftUI. Quand les bindings seront générés, on ajoutera
        // ici la dépendance vers le binaryTarget XCFramework + le wrapper Swift :
        //   .binaryTarget(name: "ChampiniumCoreFFI", path: "../../bindings/swift/ChampiniumCoreFFI.xcframework")
        //   .target(name: "ChampiniumCore", dependencies: ["ChampiniumCoreFFI"], path: "Generated")
        .executableTarget(
            name: "Champinium",
            path: "Sources/Champinium"
        )
    ]
)
