//! Stockage froid optionnel (Arweave) — ADR 0008.
//!
//! Gaté par la feature cargo `cold-storage` (aucune dépendance tirée par
//! défaut). Ce module définit le trait `ColdStore` (repli de **récupération**
//! seule) et le backend Arweave [`arweave::ArweaveColdStore`].
//!
//! **Périmètre livré (CS-a) : récupération/repli uniquement.** L'archivage
//! Arweave (signature RSA-PSS, deep-hash, upload de transaction) est **différé
//! d'implémentation** : la seule voie viable passait par la crate `rsa`, dont
//! toute version disponible est vulnérable (RUSTSEC-2023-0071, Marvin) — la
//! 0.9 stable sans mitigation, la 0.10 encore en pré-release. Aucune
//! dépendance `rsa` n'est donc tirée, et `cargo deny` reste propre **sans
//! aucun ignore ajouté**. La décision de conception (ADR 0008) tient ; seule
//! son implémentation attend une voie sans CVE (`rsa` 0.10.0 stable, ou une
//! crate Arweave maintenue). Voir `.superpowers/sdd/coldstore-decision.md`.

pub mod arweave;

use crate::error::Result as CoreResult;
use cid::Cid;

/// Contrat commun à tout backend de stockage froid (Arweave aujourd'hui,
/// Filecoin envisageable plus tard sans toucher au reste — voir la spec).
///
/// Réduit à la **récupération** : le filet de dernier recours (`retrieve`) est
/// implémenté par [`arweave::ArweaveColdStore`]. L'archivage (upload signé)
/// est différé (voir le module) et n'apparaît donc pas ici.
#[async_trait::async_trait]
pub trait ColdStore: Send + Sync {
    /// Récupère les octets d'un CID depuis le filet d'archive, si présent.
    /// `Ok(None)` = introuvable (gateways muettes ou en échec) — jamais une
    /// erreur pour une simple absence, cf. spec « repli, pas un CDN ».
    async fn retrieve(&self, cid: Cid) -> CoreResult<Option<Vec<u8>>>;
}
