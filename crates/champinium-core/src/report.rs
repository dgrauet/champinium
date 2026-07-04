//! Signalement P2P des contenus bloqués (Phase 5).
//!
//! Quand la modération refuse un contenu au checkpoint #2 (réception), le nœud
//! émet un **rapport signé** `champinium-report/v1` sur un topic gossip dédié.
//! Chaque nœud agrège localement (borné) le nombre de **rapporteurs distincts**
//! par CID : c'est de la matière première pour les éditeurs de denylists
//! (modération fédérée), pas une sanction automatique — un rapport n'a aucun
//! effet direct sur le contenu chez les pairs.

use crate::content::push_field;
use crate::error::{CoreError, Result as CoreResult};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use cid::Cid;
use libp2p::identity::{Keypair, PublicKey};
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Identifiant de schéma des rapports.
pub const SCHEMA: &str = "champinium-report/v1";

/// Taille max de la raison d'un rapport (anti-abus : le topic n'est pas un
/// canal de données arbitraires).
pub const MAX_REASON_LEN: usize = 1024;

/// Rapport signé : « ce CID a été refusé par ma modération ».
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    /// Identifiant de schéma ; doit valoir [`SCHEMA`].
    pub schema: String,
    /// CID du contenu refusé (chaîne CIDv1).
    pub cid: String,
    /// Raison courte (ex. « denylist »).
    pub reason: String,
    /// Clé publique Ed25519 du rapporteur (protobuf libp2p, base64).
    pub reporter_pubkey: String,
    /// Signature Ed25519 (base64) sur les octets canoniques.
    pub signature: Option<String>,
}

impl Report {
    /// Octets canoniques signés — champs **préfixés par longueur** (même
    /// anti-malléabilité que denylists et feeds).
    fn signing_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        push_field(&mut buf, self.schema.as_bytes());
        push_field(&mut buf, self.cid.as_bytes());
        push_field(&mut buf, self.reason.as_bytes());
        buf
    }

    /// Construit et signe un rapport.
    pub fn build_signed(reporter: &Keypair, cid: &Cid, reason: &str) -> CoreResult<Self> {
        let mut report = Self {
            schema: SCHEMA.to_string(),
            cid: cid.to_string(),
            reason: reason.to_string(),
            reporter_pubkey: B64.encode(reporter.public().encode_protobuf()),
            signature: None,
        };
        let sig = reporter
            .sign(&report.signing_bytes())
            .map_err(|e| CoreError::Moderation(format!("signature du rapport: {e}")))?;
        report.signature = Some(B64.encode(sig));
        Ok(report)
    }

    /// Parse un rapport depuis du JSON.
    pub fn from_json(json: &[u8]) -> CoreResult<Self> {
        serde_json::from_slice(json).map_err(|e| CoreError::Moderation(format!("rapport: {e}")))
    }

    /// Sérialise en JSON.
    pub fn to_json(&self) -> CoreResult<String> {
        serde_json::to_string(self).map_err(|e| CoreError::Moderation(format!("rapport: {e}")))
    }

    /// Vérifie schéma, bornes et **signature**.
    pub fn verify(&self) -> CoreResult<()> {
        if self.schema != SCHEMA {
            return Err(CoreError::Moderation(format!(
                "schéma de rapport inconnu: {}",
                self.schema
            )));
        }
        if self.reason.len() > MAX_REASON_LEN {
            return Err(CoreError::Moderation(
                "raison de rapport trop longue".into(),
            ));
        }
        self.cid()?; // le CID doit être valide
        let sig_b64 = self
            .signature
            .as_ref()
            .ok_or_else(|| CoreError::Moderation("rapport non signé".into()))?;
        let sig = B64
            .decode(sig_b64)
            .map_err(|e| CoreError::Moderation(format!("signature base64: {e}")))?;
        let pk = self.reporter_public_key()?;
        if pk.verify(&self.signing_bytes(), &sig) {
            Ok(())
        } else {
            Err(CoreError::Moderation(
                "signature de rapport invalide".into(),
            ))
        }
    }

    /// CID du contenu signalé.
    pub fn cid(&self) -> CoreResult<Cid> {
        self.cid.parse::<Cid>().map_err(CoreError::Cid)
    }

    /// Clé publique du rapporteur.
    fn reporter_public_key(&self) -> CoreResult<PublicKey> {
        let pk_bytes = B64
            .decode(&self.reporter_pubkey)
            .map_err(|e| CoreError::Moderation(format!("clé base64: {e}")))?;
        PublicKey::try_decode_protobuf(&pk_bytes)
            .map_err(|e| CoreError::Moderation(format!("clé invalide: {e}")))
    }

    /// PeerId du rapporteur (dérivé de sa clé publique vérifiée).
    pub fn reporter_peer_id(&self) -> CoreResult<PeerId> {
        Ok(self.reporter_public_key()?.to_peer_id())
    }
}

/// Borne du nombre de CIDs suivis par l'agrégateur (anti-DoS mémoire).
pub const DEFAULT_MAX_REPORTED_CIDS: usize = 10_000;
/// Borne du nombre de rapporteurs distincts retenus par CID.
pub const DEFAULT_MAX_REPORTERS_PER_CID: usize = 1_000;

/// Agrégateur local de rapports : rapporteurs **distincts** par CID, borné
/// (un CID inconnu est refusé quand l'agrégateur est plein — pas d'éviction,
/// même rationale que le catalogue : des clés jetables ne doivent pas pouvoir
/// chasser les entrées légitimes).
#[derive(Debug, Default)]
pub struct ReportBook {
    reporters: HashMap<Cid, HashSet<PeerId>>,
}

impl ReportBook {
    /// Applique un rapport **déjà vérifié**. Renvoie `true` si l'agrégat a
    /// changé (nouveau rapporteur pour ce CID).
    pub fn apply(&mut self, report: &Report) -> CoreResult<bool> {
        let cid = report.cid()?;
        let reporter = report.reporter_peer_id()?;
        if !self.reporters.contains_key(&cid) && self.reporters.len() >= DEFAULT_MAX_REPORTED_CIDS {
            return Ok(false);
        }
        let set = self.reporters.entry(cid).or_default();
        if set.len() >= DEFAULT_MAX_REPORTERS_PER_CID {
            return Ok(false);
        }
        Ok(set.insert(reporter))
    }

    /// Nombre de rapporteurs distincts pour un CID.
    pub fn count(&self, cid: &Cid) -> usize {
        self.reporters.get(cid).map_or(0, HashSet::len)
    }

    /// CIDs signalés avec leur nombre de rapporteurs distincts.
    pub fn counts(&self) -> Vec<(Cid, usize)> {
        self.reporters
            .iter()
            .map(|(cid, set)| (*cid, set.len()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::cid_for;

    #[test]
    fn same_reporter_counts_once() {
        let reporter = Keypair::generate_ed25519();
        let cid = cid_for(b"x");
        let report = Report::build_signed(&reporter, &cid, "denylist").unwrap();

        let mut book = ReportBook::default();
        assert!(book.apply(&report).unwrap());
        assert!(!book.apply(&report).unwrap(), "doublon sans effet");
        assert_eq!(book.count(&cid), 1);

        let other = Keypair::generate_ed25519();
        let second = Report::build_signed(&other, &cid, "denylist").unwrap();
        assert!(book.apply(&second).unwrap());
        assert_eq!(book.count(&cid), 2);
    }

    #[test]
    fn oversized_reason_is_rejected() {
        let reporter = Keypair::generate_ed25519();
        let report =
            Report::build_signed(&reporter, &cid_for(b"y"), &"a".repeat(MAX_REASON_LEN + 1))
                .unwrap();
        assert!(report.verify().is_err());
    }

    #[test]
    fn unsigned_report_is_rejected() {
        let reporter = Keypair::generate_ed25519();
        let mut report = Report::build_signed(&reporter, &cid_for(b"z"), "denylist").unwrap();
        report.signature = None;
        assert!(report.verify().is_err());
    }
}
