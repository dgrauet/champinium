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

pub mod blockstore;
pub mod catalog;
pub mod channel_link;
pub mod content;
pub mod error;
pub mod feed;
pub mod ffi;
pub mod identity;
pub mod ingest;
pub mod moderation;
pub mod p2p;
pub mod paths;
pub mod relay;
pub mod report;
pub mod seeding;

pub use blockstore::Blockstore;
pub use catalog::{Catalog, CatalogEntry};
pub use error::{CoreError, Result};
pub use feed::Feed;
pub use ingest::HlsManifest;
pub use moderation::{Denylist, Moderation};
pub use p2p::Node;
pub use relay::{start_relay, RelayHandle};
pub use report::Report;
pub use seeding::{SeedIndex, SeededPublication};

// Réexports pratiques pour les consommateurs Rust du crate (cli, front Linux).
pub use cid::Cid;
pub use libp2p::{Multiaddr, PeerId};

/// Version de la SURFACE de contrat UniFFI (distincte de la version du paquet).
/// Tout changement de la surface exportée incrémente cette constante ET est
/// annoncé dans AGENTS.md (voir « Protocole de changement de contrat »).
pub const CONTRACT_VERSION: u32 = 8;

/// CONTRAT v0 — version du noyau. Première fonction exposée : valide de bout en
/// bout la chaîne de génération et de chargement des bindings sur les 3 fronts.
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// CONTRAT v0 — surface du contrat exposée aux fronts (pour vérif de compat).
#[uniffi::export]
pub fn contract_version() -> u32 {
    CONTRACT_VERSION
}

/// CONTRAT v0 — coup de sonde ASYNC. Attend brièvement puis répond, prouvant que
/// l'async traverse FFI de bout en bout vers Swift ET C# (risque technique #1).
/// Sert de stub de contrat pour débloquer les agents UI en parallèle ; sera
/// remplacé par les vraies fonctions async/streams du noyau aux phases 0+.
#[uniffi::export(async_runtime = "tokio")]
pub async fn core_handshake(client: String) -> String {
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    format!("champinium-core v{} ↔ {client}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_version_matches_package() {
        assert_eq!(core_version(), env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn handshake_echoes_client() {
        let out = core_handshake("test".to_string()).await;
        assert!(out.contains("test"));
        assert!(out.contains("champinium-core"));
    }
}
