// Champinium — front macOS (SwiftUI). Présentation uniquement : toute la logique
// vit dans le noyau Rust, consommé via les bindings UniFFI (module ChampiniumCore).
import SwiftUI

@main
struct ChampiniumApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
        .defaultSize(width: 720, height: 520)
    }
}
