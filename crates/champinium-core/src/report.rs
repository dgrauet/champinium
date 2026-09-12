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

/// Borne du nombre de CIDs suivis par l'étage « autres » (anti-DoS mémoire).
pub const DEFAULT_MAX_REPORTED_CIDS: usize = 10_000;
/// Borne du nombre de rapporteurs distincts retenus par CID, étage « autres ».
pub const DEFAULT_MAX_REPORTERS_PER_CID: usize = 1_000;
/// Borne du nombre de CIDs suivis par l'étage **de confiance**. Pas de borne
/// par CID à cet étage : les clés de confiance sont peu nombreuses et choisies
/// explicitement par l'utilisateur, elles ne peuvent pas noyer le livre.
pub const MAX_TRUSTED_REPORTED_CIDS: usize = 10_000;

/// Compte de rapporteurs distincts d'un CID, **séparé par étage de confiance**.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReportTally {
    /// Rapporteurs de confiance (éditeurs de denylist souscrits).
    pub trusted: usize,
    /// Tous les autres rapporteurs (identités gratuites, gonflables).
    pub others: usize,
}

impl ReportTally {
    /// Total brut, tous étages confondus. À ne lire qu'avec la première
    /// colonne sous les yeux : seul `trusted` résiste aux clés jetables.
    pub fn total(&self) -> usize {
        self.trusted + self.others
    }
}

/// Résultat d'une insertion dans un étage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Insert {
    /// Rapporteur ajouté (l'agrégat a changé).
    Added,
    /// Rapporteur déjà présent à cet étage.
    Duplicate,
    /// Refusé par une borne de l'étage.
    Refused,
}

/// Agrégateur local de rapports : rapporteurs **distincts** par CID, borné
/// (un CID inconnu est refusé quand l'étage est plein — pas d'éviction, même
/// rationale que le catalogue : des clés jetables ne doivent pas pouvoir
/// chasser les entrées légitimes).
///
/// **Deux étages** : les rapporteurs **de confiance** sont comptés à part des
/// autres, avec leurs propres bornes — un rapport de confiance n'est jamais
/// refusé parce que l'étage « autres » déborde. L'ensemble de confiance est
/// celui des **éditeurs de denylist souscrits** : local et subjectif (ce nœud
/// les a choisis), jamais une autorité globale. Les identités Ed25519 étant
/// gratuites, un compteur unique serait trivialement gonflable (sybil) ;
/// séparer les étages rend la première colonne coûteuse à falsifier.
#[derive(Debug, Default)]
pub struct ReportBook {
    /// Clés de confiance courantes (éditeurs de denylist souscrits).
    trusted_keys: HashSet<PeerId>,
    /// Rapporteurs de confiance par CID.
    trusted: HashMap<Cid, HashSet<PeerId>>,
    /// Tous les autres rapporteurs par CID.
    others: HashMap<Cid, HashSet<PeerId>>,
}

impl ReportBook {
    /// Livre neuf avec un ensemble initial de clés de confiance.
    pub fn with_trusted(keys: HashSet<PeerId>) -> Self {
        Self {
            trusted_keys: keys,
            ..Self::default()
        }
    }

    /// Applique un rapport **déjà vérifié**. Renvoie `true` si l'agrégat a
    /// changé (nouveau rapporteur pour ce CID, à son étage).
    pub fn apply(&mut self, report: &Report) -> CoreResult<bool> {
        let cid = report.cid()?;
        let reporter = report.reporter_peer_id()?;
        let inserted = if self.trusted_keys.contains(&reporter) {
            insert_into(
                &mut self.trusted,
                cid,
                reporter,
                MAX_TRUSTED_REPORTED_CIDS,
                None,
            )
        } else {
            insert_into(
                &mut self.others,
                cid,
                reporter,
                DEFAULT_MAX_REPORTED_CIDS,
                Some(DEFAULT_MAX_REPORTERS_PER_CID),
            )
        };
        Ok(inserted == Insert::Added)
    }

    /// Remplace l'ensemble des clés de confiance et **reclasse** les
    /// rapporteurs déjà agrégés dans le bon étage (fonction pure, aucun
    /// réseau) — appelée à chaque changement d'abonnement aux éditeurs de
    /// denylist.
    ///
    /// Un rapporteur que l'étage de destination refuse (borne atteinte) est
    /// **abandonné** : le reclassement ne peut pas faire déborder un étage, et
    /// un rapport perdu ici sera réappliqué au prochain signalement du même
    /// pair. Le cas est journalisé en `debug`.
    pub fn retrust(&mut self, keys: &HashSet<PeerId>) {
        self.trusted_keys = keys.clone();
        // Extraire des deux étages AVANT de réinsérer : la place libérée par
        // les départs profite aux arrivées (sinon un étage plein refuserait
        // des rapporteurs qu'il s'apprête justement à libérer).
        let demoted = drain_if(&mut self.trusted, |peer| !keys.contains(peer));
        let promoted = drain_if(&mut self.others, |peer| keys.contains(peer));

        for (cid, reporter) in promoted {
            if insert_into(
                &mut self.trusted,
                cid,
                reporter,
                MAX_TRUSTED_REPORTED_CIDS,
                None,
            ) == Insert::Refused
            {
                tracing::debug!("reclassement: rapport de confiance {reporter} sur {cid} abandonné (étage plein)");
            }
        }
        for (cid, reporter) in demoted {
            if insert_into(
                &mut self.others,
                cid,
                reporter,
                DEFAULT_MAX_REPORTED_CIDS,
                Some(DEFAULT_MAX_REPORTERS_PER_CID),
            ) == Insert::Refused
            {
                tracing::debug!(
                    "reclassement: rapport {reporter} sur {cid} abandonné (étage autres plein)"
                );
            }
        }
    }

    /// Rapporteurs distincts d'un CID, par étage.
    pub fn tally(&self, cid: &Cid) -> ReportTally {
        ReportTally {
            trusted: self.trusted.get(cid).map_or(0, HashSet::len),
            others: self.others.get(cid).map_or(0, HashSet::len),
        }
    }

    /// CIDs signalés avec leur tally. Un CID présent à un seul étage n'est
    /// listé qu'une fois (l'autre étage vaut 0).
    pub fn tallies(&self) -> Vec<(Cid, ReportTally)> {
        let cids: HashSet<Cid> = self
            .trusted
            .keys()
            .chain(self.others.keys())
            .copied()
            .collect();
        cids.into_iter()
            .map(|cid| (cid, self.tally(&cid)))
            .collect()
    }
}

/// Insère un rapporteur dans un étage, sous ses bornes (`max_reporters` à
/// `None` = pas de borne par CID).
fn insert_into(
    tier: &mut HashMap<Cid, HashSet<PeerId>>,
    cid: Cid,
    reporter: PeerId,
    max_cids: usize,
    max_reporters: Option<usize>,
) -> Insert {
    if !tier.contains_key(&cid) && tier.len() >= max_cids {
        return Insert::Refused;
    }
    let set = tier.entry(cid).or_default();
    if max_reporters.is_some_and(|max| set.len() >= max) {
        return Insert::Refused;
    }
    if set.insert(reporter) {
        Insert::Added
    } else {
        Insert::Duplicate
    }
}

/// Retire d'un étage tous les rapporteurs satisfaisant `pred` et les renvoie,
/// en supprimant les ensembles devenus vides.
fn drain_if(
    tier: &mut HashMap<Cid, HashSet<PeerId>>,
    pred: impl Fn(&PeerId) -> bool,
) -> Vec<(Cid, PeerId)> {
    let mut moved = Vec::new();
    tier.retain(|cid, set| {
        let leaving: Vec<PeerId> = set.iter().filter(|p| pred(p)).copied().collect();
        for peer in leaving {
            set.remove(&peer);
            moved.push((*cid, peer));
        }
        !set.is_empty()
    });
    moved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::cid_for;

    fn signed(reporter: &Keypair, cid: &Cid) -> Report {
        Report::build_signed(reporter, cid, "denylist").unwrap()
    }

    #[test]
    fn same_reporter_counts_once() {
        let reporter = Keypair::generate_ed25519();
        let cid = cid_for(b"x");
        let report = Report::build_signed(&reporter, &cid, "denylist").unwrap();

        let mut book = ReportBook::default();
        assert!(book.apply(&report).unwrap());
        assert!(!book.apply(&report).unwrap(), "doublon sans effet");
        assert_eq!(book.tally(&cid).others, 1);

        let other = Keypair::generate_ed25519();
        let second = Report::build_signed(&other, &cid, "denylist").unwrap();
        assert!(book.apply(&second).unwrap());
        assert_eq!(
            book.tally(&cid),
            ReportTally {
                trusted: 0,
                others: 2
            }
        );
    }

    #[test]
    fn trusted_and_other_reporters_are_tallied_separately() {
        let trusted_kp = Keypair::generate_ed25519();
        let other_kp = Keypair::generate_ed25519();
        let cid = cid_for(b"t1");
        let mut book = ReportBook::with_trusted([trusted_kp.public().to_peer_id()].into());
        assert!(book.apply(&signed(&trusted_kp, &cid)).unwrap());
        assert!(book.apply(&signed(&other_kp, &cid)).unwrap());
        assert!(
            !book.apply(&signed(&other_kp, &cid)).unwrap(),
            "doublon sans effet"
        );
        assert_eq!(
            book.tally(&cid),
            ReportTally {
                trusted: 1,
                others: 1
            }
        );
        assert_eq!(book.tally(&cid).total(), 2);
    }

    #[test]
    fn trusted_report_enters_even_when_other_tier_is_full_for_that_cid() {
        let cid = cid_for(b"t2");
        let trusted_kp = Keypair::generate_ed25519();
        let mut book = ReportBook::with_trusted([trusted_kp.public().to_peer_id()].into());
        for _ in 0..DEFAULT_MAX_REPORTERS_PER_CID {
            book.apply(&signed(&Keypair::generate_ed25519(), &cid))
                .unwrap();
        }
        assert!(
            !book
                .apply(&signed(&Keypair::generate_ed25519(), &cid))
                .unwrap(),
            "étage autres plein"
        );
        assert!(
            book.apply(&signed(&trusted_kp, &cid)).unwrap(),
            "la confiance passe"
        );
        assert_eq!(book.tally(&cid).trusted, 1);
    }

    #[test]
    fn trusted_report_enters_even_when_other_tier_is_full_of_cids() {
        let trusted_kp = Keypair::generate_ed25519();
        let mut book = ReportBook::with_trusted([trusted_kp.public().to_peer_id()].into());
        let filler = Keypair::generate_ed25519();
        for i in 0..DEFAULT_MAX_REPORTED_CIDS {
            book.apply(&signed(&filler, &cid_for(format!("fill-{i}").as_bytes())))
                .unwrap();
        }
        let fresh = cid_for(b"fresh");
        assert!(
            !book.apply(&signed(&filler, &fresh)).unwrap(),
            "livre autres plein"
        );
        assert!(book.apply(&signed(&trusted_kp, &fresh)).unwrap());
        assert_eq!(
            book.tally(&fresh),
            ReportTally {
                trusted: 1,
                others: 0
            }
        );
    }

    #[test]
    fn retrust_moves_reporters_between_tiers_without_duplicates() {
        let kp = Keypair::generate_ed25519();
        let pid = kp.public().to_peer_id();
        let cid = cid_for(b"t4");
        let mut book = ReportBook::with_trusted(HashSet::new());
        book.apply(&signed(&kp, &cid)).unwrap();
        assert_eq!(
            book.tally(&cid),
            ReportTally {
                trusted: 0,
                others: 1
            }
        );
        book.retrust(&[pid].into());
        assert_eq!(
            book.tally(&cid),
            ReportTally {
                trusted: 1,
                others: 0
            }
        );
        book.retrust(&HashSet::new());
        assert_eq!(
            book.tally(&cid),
            ReportTally {
                trusted: 0,
                others: 1
            }
        );
        assert_eq!(book.tallies().len(), 1);
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
