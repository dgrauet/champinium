//! champinium-core — noyau Rust partagé de Champinium.
//!
//! SQUELETTE : aucune logique métier. Toute la logique (P2P, content-addressing,
//! ingestion ffmpeg, catalogue CRDT, modération) viendra vivre ICI et sera exposée
//! aux 3 fronts natifs via UniFFI. Les fronts ne contiennent QUE de la présentation.
//!
//! Le découpage cible des modules (à implémenter phase par phase) :
//!   - identity   : paire de clés Ed25519 -> PeerID/DID, signature.
//!   - p2p        : Swarm rust-libp2p (Kademlia, gossipsub, bitswap, relay, DCUtR).
//!   - blockstore : stockage content-addressed (CID) + cache LRU.
//!   - ingest     : orchestration ffmpeg -> segmentation HLS -> CIDs.
//!   - catalog    : CRDT maison reconstruit par écoute gossipsub.
//!   - moderation : hash-matching + denylists signées (checkpoints #1 et #2).
//!   - player     : résolution CID -> manifest/segments HLS pour le player natif.
//!
//! ⚠️ RISQUE TECHNIQUE #1 — async via FFI : l'exposition de fonctions async et de
//! streams d'événements tokio vers Swift ET C# doit être prototypée TÔT (Phase 0).

uniffi::setup_scaffolding!();

/// Version du noyau. Première fonction exposée : sert à valider de bout en bout
/// la chaîne de génération et de chargement des bindings sur les 3 fronts.
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// PHASE 0 (de-risking) viendra ici : une `async fn` et un `Stream<Event>` tokio
// exposés via UniFFI, à consommer depuis un vrai binaire Swift ET C#.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_version_matches_package() {
        assert_eq!(core_version(), env!("CARGO_PKG_VERSION"));
    }
}
