//! champinium-bootstrap — nœud de rendez-vous initial (SQUELETTE).
//!
//! PRINCIPE : SANS ÉTAT. Aucune base, aucune donnée persistée au fonctionnement.
//! Sert uniquement de point de rendez-vous initial pour la découverte de pairs
//! (Kademlia bootstrap). Doit rester multipliable trivialement : n'importe qui
//! peut lancer le sien (procédure documentée dans le README).
fn main() {
    println!(
        "champinium-bootstrap (squelette, stateless) — noyau v{}",
        champinium_core::core_version()
    );
    println!("TODO Phase 1 : démarrer un Swarm libp2p en mode bootstrap (Kademlia).");
}
