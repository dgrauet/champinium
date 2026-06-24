//! champinium-cli — outil de debug du noyau (SQUELETTE).
//!
//! Phase 1 cible : `add`, `get`, `provide`, `findprovs` contre un Swarm libp2p réel.
//! Pour l'instant, ne fait que prouver le lien avec le noyau partagé.
fn main() {
    println!(
        "champinium-cli (squelette) — noyau v{}",
        champinium_core::core_version()
    );
    println!("TODO Phase 1 : add / get / provide / findprovs");
}
