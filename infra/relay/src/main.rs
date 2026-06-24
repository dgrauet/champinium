//! champinium-relay — relay NAT traversal (SQUELETTE).
//!
//! PRINCIPE : SANS ÉTAT. Fournit du circuit relay v2 + assiste le hole punching
//! (DCUtR) pour les pairs derrière NAT. Aucune donnée persistée. Multipliable
//! trivialement : n'importe qui peut lancer le sien (procédure dans le README).
fn main() {
    println!(
        "champinium-relay (squelette, stateless) — noyau v{}",
        champinium_core::core_version()
    );
    println!("TODO Phase 4 : circuit relay v2 + DCUtR.");
}
