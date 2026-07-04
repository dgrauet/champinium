//! Feed mutable signé d'un créateur.
//!
//! Un feed est la liste, **signée par l'identité du créateur** (Ed25519), des CIDs
//! qu'il publie. Il est **versionné** par un `seq` monotone : à chaque mise à jour,
//! le créateur réémet un feed avec un `seq` plus grand. La résolution de conflit
//! est « le plus grand `seq` gagne » (last-writer-wins), la signature garantissant
//! l'authenticité. Diffusé en gossipsub (live) ; le catalogue est reconstruit en
//! écoutant (voir [`crate::catalog`]).

use crate::content::push_field;
use crate::error::{CoreError, Result as CoreResult};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use cid::Cid;
use libp2p::identity::{Keypair, PublicKey};
use libp2p::PeerId;
use serde::{Deserialize, Serialize};

/// Identifiant de schéma de feed v1 (CIDs nus, sans métadonnées) — conservé en
/// **lecture** (les feeds v1 existants restent valides) ; les producteurs
/// émettent du v2.
pub const SCHEMA: &str = "champinium-feed/v1";
/// Identifiant de schéma de feed v2 : chaque contenu porte des métadonnées
/// signées (titre, tags) — le socle de la recherche décentralisée.
pub const SCHEMA_V2: &str = "champinium-feed/v2";

/// Bornes des métadonnées (anti-abus : le feed n'est pas un canal de données
/// arbitraires ; ces bornes sont VÉRIFIÉES à la réception, pas seulement à la
/// construction).
pub const MAX_TITLE_LEN: usize = 256;
pub const MAX_TAG_LEN: usize = 64;
pub const MAX_TAGS_PER_ENTRY: usize = 16;

/// Un contenu publié dans un feed v2 : son CID et ses métadonnées signées.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedEntry {
    /// CID du contenu (chaîne CIDv1) — pour une vidéo, le manifeste HLS.
    pub cid: String,
    /// Titre lisible (peut être vide).
    pub title: String,
    /// Tags normalisés (minuscules, sans espaces de bord).
    pub tags: Vec<String>,
}

/// Feed signé d'un créateur (formats `champinium-feed/v1` et `/v2`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feed {
    /// Identifiant de schéma ; [`SCHEMA`] (v1) ou [`SCHEMA_V2`].
    pub schema: String,
    /// Clé publique Ed25519 de l'émetteur (protobuf libp2p, base64).
    pub issuer_pubkey: String,
    /// Numéro de version monotone (le plus grand gagne).
    pub seq: u64,
    /// CIDs publiés (v1 uniquement ; vide en v2, où tout passe par `entries`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cids: Vec<String>,
    /// Contenus publiés avec métadonnées (v2 uniquement ; vide en v1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<FeedEntry>,
    /// Signature Ed25519 (base64) sur les octets canoniques du feed.
    pub signature: Option<String>,
}

/// Normalise un tag : minuscules, sans espaces de bord.
pub fn normalize_tag(tag: &str) -> String {
    tag.trim().to_lowercase()
}

impl Feed {
    /// Octets canoniques signés (déterministes). Chaque champ est **préfixé par
    /// sa longueur** (non séparé par `\n`) pour éliminer toute malléabilité par
    /// décalage de frontière. v1 : CIDs triés (octets inchangés — les feeds v1
    /// existants restent vérifiables). v2 : entrées triées par CID, chacune
    /// couvrant cid, titre et tags.
    fn signing_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        push_field(&mut buf, self.schema.as_bytes());
        push_field(&mut buf, self.issuer_pubkey.as_bytes());
        push_field(&mut buf, &self.seq.to_le_bytes());
        if self.schema == SCHEMA_V2 {
            let mut entries = self.entries.clone();
            entries.sort_by(|a, b| a.cid.cmp(&b.cid));
            push_field(&mut buf, &(entries.len() as u64).to_le_bytes());
            for e in entries {
                push_field(&mut buf, e.cid.as_bytes());
                push_field(&mut buf, e.title.as_bytes());
                push_field(&mut buf, &(e.tags.len() as u64).to_le_bytes());
                for t in &e.tags {
                    push_field(&mut buf, t.as_bytes());
                }
            }
        } else {
            let mut cids = self.cids.clone();
            cids.sort();
            push_field(&mut buf, &(cids.len() as u64).to_le_bytes());
            for c in cids {
                push_field(&mut buf, c.as_bytes());
            }
        }
        buf
    }

    /// Construit et **signe** un feed v2 sans métadonnées (titres vides). API de
    /// commodité pour les publications par CIDs nus.
    pub fn build_signed(issuer: &Keypair, seq: u64, cids: &[Cid]) -> CoreResult<Self> {
        let entries: Vec<FeedEntry> = cids
            .iter()
            .map(|c| FeedEntry {
                cid: c.to_string(),
                title: String::new(),
                tags: Vec::new(),
            })
            .collect();
        Self::build_signed_with(issuer, seq, &entries)
    }

    /// Construit et **signe** un feed v2 avec métadonnées. Les tags sont
    /// normalisés (minuscules, sans espaces de bord ; les vides disparaissent).
    pub fn build_signed_with(
        issuer: &Keypair,
        seq: u64,
        entries: &[FeedEntry],
    ) -> CoreResult<Self> {
        let entries: Vec<FeedEntry> = entries
            .iter()
            .map(|e| FeedEntry {
                cid: e.cid.clone(),
                title: e.title.clone(),
                tags: e
                    .tags
                    .iter()
                    .map(|t| normalize_tag(t))
                    .filter(|t| !t.is_empty())
                    .collect(),
            })
            .collect();
        let mut feed = Self {
            schema: SCHEMA_V2.to_string(),
            issuer_pubkey: B64.encode(issuer.public().encode_protobuf()),
            seq,
            cids: Vec::new(),
            entries,
            signature: None,
        };
        let sig = issuer
            .sign(&feed.signing_bytes())
            .map_err(|e| CoreError::Network(format!("signature feed: {e}")))?;
        feed.signature = Some(B64.encode(sig));
        Ok(feed)
    }

    /// Construit et signe un feed **v1** (CIDs nus). Conservé pour les tests de
    /// compatibilité de lecture — les producteurs émettent du v2.
    pub fn build_signed_v1(issuer: &Keypair, seq: u64, cids: &[Cid]) -> CoreResult<Self> {
        let mut feed = Self {
            schema: SCHEMA.to_string(),
            issuer_pubkey: B64.encode(issuer.public().encode_protobuf()),
            seq,
            cids: cids.iter().map(|c| c.to_string()).collect(),
            entries: Vec::new(),
            signature: None,
        };
        let sig = issuer
            .sign(&feed.signing_bytes())
            .map_err(|e| CoreError::Network(format!("signature feed: {e}")))?;
        feed.signature = Some(B64.encode(sig));
        Ok(feed)
    }

    /// Sérialise en JSON.
    pub fn to_json(&self) -> CoreResult<String> {
        serde_json::to_string(self).map_err(|e| CoreError::Network(format!("json feed: {e}")))
    }

    /// Parse depuis JSON.
    pub fn from_json(json: &[u8]) -> CoreResult<Self> {
        serde_json::from_slice(json).map_err(|e| CoreError::Network(format!("json feed: {e}")))
    }

    /// Clé publique décodée de l'émetteur.
    fn public_key(&self) -> CoreResult<PublicKey> {
        let bytes = B64
            .decode(&self.issuer_pubkey)
            .map_err(|e| CoreError::Network(format!("clé base64: {e}")))?;
        PublicKey::try_decode_protobuf(&bytes)
            .map_err(|e| CoreError::Network(format!("clé invalide: {e}")))
    }

    /// `PeerId` du créateur, dérivé de sa clé publique.
    pub fn issuer_peer_id(&self) -> CoreResult<PeerId> {
        Ok(self.public_key()?.to_peer_id())
    }

    /// Vérifie le schéma, les **bornes** et la **signature** du feed.
    pub fn verify(&self) -> CoreResult<()> {
        match self.schema.as_str() {
            // v1 : pas de métadonnées — des `entries` présentes seraient HORS
            // signature (malléables), donc rejetées.
            s if s == SCHEMA => {
                if !self.entries.is_empty() {
                    return Err(CoreError::Network(
                        "feed v1 avec entries non signées".into(),
                    ));
                }
            }
            // v2 : tout passe par `entries` (bornées) ; `cids` doit être vide.
            s if s == SCHEMA_V2 => {
                if !self.cids.is_empty() {
                    return Err(CoreError::Network("feed v2 avec cids hors entries".into()));
                }
                for e in &self.entries {
                    if e.title.len() > MAX_TITLE_LEN
                        || e.tags.len() > MAX_TAGS_PER_ENTRY
                        || e.tags.iter().any(|t| t.len() > MAX_TAG_LEN)
                    {
                        return Err(CoreError::Network("métadonnées de feed hors bornes".into()));
                    }
                }
            }
            other => {
                return Err(CoreError::Network(format!("schéma feed inconnu: {other}")));
            }
        }
        let sig_b64 = self
            .signature
            .as_ref()
            .ok_or_else(|| CoreError::Network("feed non signé".into()))?;
        let sig = B64
            .decode(sig_b64)
            .map_err(|e| CoreError::Network(format!("signature base64: {e}")))?;
        if self.public_key()?.verify(&self.signing_bytes(), &sig) {
            Ok(())
        } else {
            Err(CoreError::Network("signature feed invalide".into()))
        }
    }

    /// CIDs du feed (après parsing) — v1 : champ `cids` ; v2 : depuis `entries`.
    pub fn cids(&self) -> CoreResult<Vec<Cid>> {
        if self.schema == SCHEMA_V2 {
            self.entries
                .iter()
                .map(|e| e.cid.parse::<Cid>().map_err(CoreError::Cid))
                .collect()
        } else {
            self.cids
                .iter()
                .map(|c| c.parse::<Cid>().map_err(CoreError::Cid))
                .collect()
        }
    }

    /// Tags distincts (normalisés) portés par le feed, tous contenus confondus.
    pub fn all_tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self
            .entries
            .iter()
            .flat_map(|e| e.tags.iter().cloned())
            .collect();
        tags.sort();
        tags.dedup();
        tags
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::cid_for;

    #[test]
    fn build_verify_roundtrip() {
        let issuer = Keypair::generate_ed25519();
        let feed = Feed::build_signed(&issuer, 1, &[cid_for(b"a"), cid_for(b"b")]).unwrap();
        let json = feed.to_json().unwrap();
        let parsed = Feed::from_json(json.as_bytes()).unwrap();
        parsed.verify().unwrap();
        assert_eq!(
            parsed.issuer_peer_id().unwrap(),
            issuer.public().to_peer_id()
        );
        assert_eq!(parsed.cids().unwrap().len(), 2);
    }

    #[test]
    fn tampered_feed_fails_verification() {
        let issuer = Keypair::generate_ed25519();
        let mut feed = Feed::build_signed(&issuer, 1, &[cid_for(b"a")]).unwrap();
        feed.cids.push(cid_for(b"injecte").to_string());
        assert!(feed.verify().is_err());
    }

    #[test]
    fn bumping_seq_requires_resigning() {
        let issuer = Keypair::generate_ed25519();
        let mut feed = Feed::build_signed(&issuer, 1, &[cid_for(b"a")]).unwrap();
        feed.seq = 2; // modifie le seq sans re-signer
        assert!(feed.verify().is_err());
    }

    // --- champinium-feed/v2 : métadonnées (titre, tags) signées ---

    fn entry(cid: Cid, title: &str, tags: &[&str]) -> FeedEntry {
        FeedEntry {
            cid: cid.to_string(),
            title: title.to_string(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
        }
    }

    #[test]
    fn v2_roundtrips_with_metadata() {
        let issuer = Keypair::generate_ed25519();
        let feed = Feed::build_signed_with(
            &issuer,
            1,
            &[entry(
                cid_for(b"a"),
                "Aurores boréales",
                &["nature", "nuit"],
            )],
        )
        .unwrap();
        assert_eq!(feed.schema, SCHEMA_V2);

        let parsed = Feed::from_json(feed.to_json().unwrap().as_bytes()).unwrap();
        parsed.verify().expect("signature v2 valide");
        assert_eq!(parsed.cids().unwrap(), vec![cid_for(b"a")]);
        assert_eq!(parsed.entries[0].title, "Aurores boréales");
        assert_eq!(parsed.entries[0].tags, vec!["nature", "nuit"]);
    }

    #[test]
    fn v2_metadata_is_covered_by_signature() {
        let issuer = Keypair::generate_ed25519();
        let mut feed =
            Feed::build_signed_with(&issuer, 1, &[entry(cid_for(b"a"), "Titre", &["tag"])])
                .unwrap();
        feed.entries[0].title = "Titre falsifié".into();
        assert!(feed.verify().is_err(), "le titre est signé");

        let mut feed2 =
            Feed::build_signed_with(&issuer, 1, &[entry(cid_for(b"a"), "Titre", &["tag"])])
                .unwrap();
        feed2.entries[0].tags.push("injecte".into());
        assert!(feed2.verify().is_err(), "les tags sont signés");
    }

    #[test]
    fn v1_feeds_still_verify_but_cannot_smuggle_entries() {
        let issuer = Keypair::generate_ed25519();
        // Un feed v1 existant (sans métadonnées) reste valide (compat lecture).
        let v1 = Feed::build_signed_v1(&issuer, 1, &[cid_for(b"a")]).unwrap();
        v1.verify().expect("un feed v1 reste lisible");

        // ... mais on ne peut pas y injecter des `entries` hors signature.
        let mut forged = v1.clone();
        forged.entries.push(entry(cid_for(b"a"), "Injecté", &[]));
        assert!(
            forged.verify().is_err(),
            "des entries non signées sur un v1 doivent être rejetées"
        );
    }

    #[test]
    fn v2_rejects_out_of_bounds_metadata() {
        let issuer = Keypair::generate_ed25519();
        // Titre trop long.
        let long_title = "t".repeat(MAX_TITLE_LEN + 1);
        let feed =
            Feed::build_signed_with(&issuer, 1, &[entry(cid_for(b"a"), &long_title, &[])]).unwrap();
        assert!(feed.verify().is_err());

        // Trop de tags.
        let many: Vec<String> = (0..MAX_TAGS_PER_ENTRY + 1)
            .map(|i| format!("tag{i}"))
            .collect();
        let many_refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let feed =
            Feed::build_signed_with(&issuer, 1, &[entry(cid_for(b"a"), "t", &many_refs)]).unwrap();
        assert!(feed.verify().is_err());
    }

    #[test]
    fn normalized_tags() {
        // Les tags sont normalisés à la construction : minuscules, sans espaces
        // de bord ; les vides disparaissent.
        let issuer = Keypair::generate_ed25519();
        let feed = Feed::build_signed_with(
            &issuer,
            1,
            &[entry(cid_for(b"a"), "T", &["  NaTure ", "", "nuit"])],
        )
        .unwrap();
        feed.verify().unwrap();
        assert_eq!(feed.entries[0].tags, vec!["nature", "nuit"]);
    }
}
