//! Moteur de modération — garde-fou OBLIGATOIRE, actif par défaut.
//!
//! Sur un réseau décentralisé, la suppression centrale est impossible : la
//! modération est donc côté nœud. Deux mécanismes :
//!
//! 1. **Denylist par défaut** compilée dans le binaire (inaltérable à l'exécution,
//!    donc non désactivable) — voir `deny/default.cids`.
//! 2. **Denylists signées souscrites** (modèle fédéré) : objets signés Ed25519
//!    qu'un nœud choisit de suivre ; leur signature est **vérifiée** avant prise
//!    en compte. Format `champinium-denylist/v2` — v1 (CIDs seuls) est supprimé :
//!    politique zéro-compat déjà appliquée aux feeds (`champinium-feed/v3`).
//!
//! L'enforcement se fait à deux checkpoints (voir [`crate::p2p::Node`]) :
//! - **#1 ingestion** : refus de publier un contenu matché ;
//! - **#2 réception/service** : refus de récupérer, mettre en cache, reseeder ou
//!   servir un contenu matché.

use crate::content::push_field;
use crate::error::{CoreError, Result as CoreResult};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use cid::Cid;
use libp2p::identity::{Keypair, PublicKey};
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::str::FromStr;

/// Identifiant de schéma de denylist.
pub const SCHEMA: &str = "champinium-denylist/v2";

/// Nombre maximal d'entrées (CIDs + clés cumulés) dans une denylist — borne
/// anti-abus, absente en v1, posée avec l'ajout des entrées de clés.
pub const MAX_DENYLIST_ENTRIES: usize = 65_536;

/// Denylist signée souscrite (format `champinium-denylist/v2`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Denylist {
    /// Identifiant de schéma ; doit valoir [`SCHEMA`].
    pub schema: String,
    /// Nom lisible de la liste.
    pub name: String,
    /// Clé publique Ed25519 de l'émetteur (protobuf libp2p, encodé base64).
    pub issuer_pubkey: String,
    /// Horodatage de mise à jour (RFC 3339).
    pub updated: String,
    /// CIDs bloqués (chaînes CIDv1).
    pub entries: Vec<String>,
    /// Clés (PeerIds, base58) bloquées entièrement — tout contenu émis par ces
    /// clés est refusé, quel que soit son CID.
    pub key_entries: Vec<String>,
    /// Signature Ed25519 (base64) sur les octets canoniques de la liste.
    pub signature: Option<String>,
}

impl Denylist {
    /// Octets canoniques signés (indépendants de la sérialisation JSON, donc
    /// déterministes) : schéma, nom, date, CIDs triés, puis clés triées. Chaque
    /// champ est **préfixé par sa longueur** (et non séparé par `\n`) pour
    /// empêcher toute malléabilité par décalage de frontière (un `\n` dans un
    /// champ ne peut plus faire passer du contenu d'un champ à l'autre à octets
    /// signés constants).
    fn signing_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        push_field(&mut buf, self.schema.as_bytes());
        push_field(&mut buf, self.name.as_bytes());
        push_field(&mut buf, self.updated.as_bytes());
        let mut entries = self.entries.clone();
        entries.sort();
        push_field(&mut buf, &(entries.len() as u64).to_le_bytes());
        for e in entries {
            push_field(&mut buf, e.as_bytes());
        }
        let mut key_entries = self.key_entries.clone();
        key_entries.sort();
        push_field(&mut buf, &(key_entries.len() as u64).to_le_bytes());
        for k in key_entries {
            push_field(&mut buf, k.as_bytes());
        }
        buf
    }

    /// Construit et **signe** une denylist (côté éditeur/publisher).
    pub fn build_signed(
        name: &str,
        updated: &str,
        issuer: &Keypair,
        entries: &[Cid],
        keys: &[PeerId],
    ) -> CoreResult<Self> {
        let mut dl = Self {
            schema: SCHEMA.to_string(),
            name: name.to_string(),
            issuer_pubkey: B64.encode(issuer.public().encode_protobuf()),
            updated: updated.to_string(),
            entries: entries.iter().map(|c| c.to_string()).collect(),
            key_entries: keys.iter().map(|k| k.to_string()).collect(),
            signature: None,
        };
        let sig = issuer
            .sign(&dl.signing_bytes())
            .map_err(|e| CoreError::Moderation(format!("signature: {e}")))?;
        dl.signature = Some(B64.encode(sig));
        Ok(dl)
    }

    /// Parse une denylist depuis du JSON.
    pub fn from_json(json: &str) -> CoreResult<Self> {
        serde_json::from_str(json).map_err(|e| CoreError::Moderation(format!("json: {e}")))
    }

    /// Vérifie le schéma, la borne anti-abus et la **signature** de la liste.
    pub fn verify(&self) -> CoreResult<()> {
        if self.schema != SCHEMA {
            return Err(CoreError::Moderation(format!(
                "schéma inconnu: {}",
                self.schema
            )));
        }
        if self.entries.len() + self.key_entries.len() > MAX_DENYLIST_ENTRIES {
            return Err(CoreError::Moderation(format!(
                "denylist trop grande: {} entrées (max {MAX_DENYLIST_ENTRIES})",
                self.entries.len() + self.key_entries.len()
            )));
        }
        let sig_b64 = self
            .signature
            .as_ref()
            .ok_or_else(|| CoreError::Moderation("denylist non signée".into()))?;
        let sig = B64
            .decode(sig_b64)
            .map_err(|e| CoreError::Moderation(format!("signature base64: {e}")))?;
        let pk_bytes = B64
            .decode(&self.issuer_pubkey)
            .map_err(|e| CoreError::Moderation(format!("clé base64: {e}")))?;
        let pk = PublicKey::try_decode_protobuf(&pk_bytes)
            .map_err(|e| CoreError::Moderation(format!("clé invalide: {e}")))?;
        if pk.verify(&self.signing_bytes(), &sig) {
            Ok(())
        } else {
            Err(CoreError::Moderation("signature invalide".into()))
        }
    }

    /// CIDs de la liste (après parsing).
    pub fn cids(&self) -> CoreResult<HashSet<Cid>> {
        self.entries
            .iter()
            .map(|e| e.parse::<Cid>().map_err(CoreError::Cid))
            .collect()
    }

    /// Clés (PeerIds) de la liste (après parsing).
    pub fn keys(&self) -> CoreResult<HashSet<PeerId>> {
        self.key_entries
            .iter()
            .map(|k| {
                PeerId::from_str(k).map_err(|e| CoreError::Moderation(format!("clé invalide: {e}")))
            })
            .collect()
    }
}

/// Denylist par défaut, compilée dans le binaire (non désactivable).
const DEFAULT_CIDS: &str = include_str!("../../../deny/default.cids");

/// Moteur de modération : ensemble des CIDs et des clés bloqués (défaut + souscriptions).
#[derive(Debug, Clone, Default)]
pub struct Moderation {
    blocked: HashSet<Cid>,
    blocked_keys: HashSet<PeerId>,
}

impl Moderation {
    /// Moteur avec la denylist par défaut active (recommandé / défaut applicatif).
    pub fn with_default() -> CoreResult<Self> {
        let mut m = Self::default();
        m.add_raw_cids(DEFAULT_CIDS)?;
        Ok(m)
    }

    /// Moteur vide (pour les tests). La denylist par défaut n'est PAS chargée.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Souscrit à une denylist signée : **vérifie la signature** puis ajoute ses
    /// CIDs et ses clés bloquées.
    pub fn subscribe(&mut self, list: &Denylist) -> CoreResult<()> {
        list.verify()?;
        self.blocked.extend(list.cids()?);
        self.blocked_keys.extend(list.keys()?);
        Ok(())
    }

    /// Indique si un CID est bloqué.
    pub fn is_blocked(&self, cid: &Cid) -> bool {
        self.blocked.contains(cid)
    }

    /// Indique si une clé (PeerId) est bloquée en entier.
    pub fn is_blocked_key(&self, peer: &PeerId) -> bool {
        self.blocked_keys.contains(peer)
    }

    /// Nombre de CIDs bloqués.
    pub fn len(&self) -> usize {
        self.blocked.len()
    }

    /// Vrai si aucun CID n'est bloqué.
    pub fn is_empty(&self) -> bool {
        self.blocked.is_empty()
    }

    /// Ajoute des CIDs depuis un texte (un CID par ligne ; `#` = commentaire).
    fn add_raw_cids(&mut self, raw: &str) -> CoreResult<()> {
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            self.blocked
                .insert(line.parse::<Cid>().map_err(CoreError::Cid)?);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::cid_for;

    #[test]
    fn default_loads_without_error_and_is_empty() {
        let m = Moderation::with_default().unwrap();
        assert!(m.is_empty(), "la denylist par défaut est vide à ce stade");
    }

    #[test]
    fn signed_denylist_roundtrips_and_blocks() {
        let issuer = Keypair::generate_ed25519();
        let bad = cid_for(b"contenu interdit");
        let dl =
            Denylist::build_signed("test", "2026-06-24T00:00:00Z", &issuer, &[bad], &[]).unwrap();

        // Re-sérialisation/parse JSON puis vérification.
        let json = serde_json::to_string(&dl).unwrap();
        let parsed = Denylist::from_json(&json).unwrap();
        parsed.verify().expect("signature valide");

        let mut m = Moderation::empty();
        m.subscribe(&parsed).unwrap();
        assert!(m.is_blocked(&bad));
        assert!(!m.is_blocked(&cid_for(b"contenu ok")));
    }

    #[test]
    fn signed_denylist_roundtrips_with_key_entries_and_blocks_key() {
        let issuer = Keypair::generate_ed25519();
        let banned_peer = PeerId::from(Keypair::generate_ed25519().public());
        let other_peer = PeerId::from(Keypair::generate_ed25519().public());
        let dl =
            Denylist::build_signed("test", "2026-07-23T00:00:00Z", &issuer, &[], &[banned_peer])
                .unwrap();

        let json = serde_json::to_string(&dl).unwrap();
        let parsed = Denylist::from_json(&json).unwrap();
        parsed.verify().expect("signature valide");

        let mut m = Moderation::empty();
        m.subscribe(&parsed).unwrap();
        assert!(m.is_blocked_key(&banned_peer));
        assert!(!m.is_blocked_key(&other_peer));
    }

    #[test]
    fn tampered_key_entries_fail_verification() {
        let issuer = Keypair::generate_ed25519();
        let banned_peer = PeerId::from(Keypair::generate_ed25519().public());
        let mut dl =
            Denylist::build_signed("t", "2026-07-23T00:00:00Z", &issuer, &[], &[]).unwrap();
        // Ajoute une clé après signature : la signature ne couvre plus les clés.
        dl.key_entries.push(banned_peer.to_string());
        assert!(dl.verify().is_err());

        let mut m = Moderation::empty();
        assert!(
            m.subscribe(&dl).is_err(),
            "une liste altérée (clé injectée) est rejetée"
        );
    }

    #[test]
    fn tampered_entries_fail_verification() {
        let issuer = Keypair::generate_ed25519();
        let mut dl =
            Denylist::build_signed("t", "2026-06-24T00:00:00Z", &issuer, &[cid_for(b"x")], &[])
                .unwrap();
        // Ajoute un CID après signature : la signature ne couvre plus les entrées.
        dl.entries.push(cid_for(b"injecte").to_string());
        assert!(dl.verify().is_err());

        let mut m = Moderation::empty();
        assert!(m.subscribe(&dl).is_err(), "une liste altérée est rejetée");
    }

    #[test]
    fn field_boundary_shifting_is_not_malleable() {
        // Denylist légitime : updated="u", entries=["cidA","cidB"] (triés).
        let issuer = Keypair::generate_ed25519();
        let a = cid_for(b"aaa");
        let b = cid_for(b"bbb");
        let (a, b) = if a.to_string() < b.to_string() {
            (a, b)
        } else {
            (b, a)
        };
        let legit = Denylist::build_signed("n", "u", &issuer, &[a, b], &[]).unwrap();

        // Attaque : on déplace le premier CID depuis `entries` vers `updated`.
        // Avec une concaténation naïve séparée par '\n', les octets signés sont
        // identiques → la même signature validerait cette liste falsifiée.
        let forged = Denylist {
            schema: legit.schema.clone(),
            name: legit.name.clone(),
            issuer_pubkey: legit.issuer_pubkey.clone(),
            updated: format!("u\n{a}"),
            entries: vec![b.to_string()],
            key_entries: legit.key_entries.clone(),
            signature: legit.signature.clone(),
        };
        assert!(
            forged.verify().is_err(),
            "un décalage de frontière de champ ne doit pas produire une signature valide"
        );
    }

    #[test]
    fn key_boundary_shifting_is_not_malleable() {
        // Attaque équivalente, mais en déplaçant une entrée entre `entries` et
        // `key_entries` — les deux collections doivent être couvertes indépendamment.
        let issuer = Keypair::generate_ed25519();
        let peer = PeerId::from(Keypair::generate_ed25519().public());
        let legit = Denylist::build_signed("n", "u", &issuer, &[], &[peer]).unwrap();

        let forged = Denylist {
            schema: legit.schema.clone(),
            name: legit.name.clone(),
            issuer_pubkey: legit.issuer_pubkey.clone(),
            updated: legit.updated.clone(),
            entries: vec![peer.to_string()],
            key_entries: vec![],
            signature: legit.signature.clone(),
        };
        assert!(
            forged.verify().is_err(),
            "déplacer une clé vers entries ne doit pas produire une signature valide"
        );
    }

    #[test]
    fn wrong_issuer_key_fails_verification() {
        let issuer = Keypair::generate_ed25519();
        let mut dl =
            Denylist::build_signed("t", "2026-06-24T00:00:00Z", &issuer, &[cid_for(b"y")], &[])
                .unwrap();
        // Remplace la clé émettrice par une autre : signature non vérifiable.
        let other = Keypair::generate_ed25519();
        dl.issuer_pubkey = B64.encode(other.public().encode_protobuf());
        assert!(dl.verify().is_err());
    }

    #[test]
    fn unsigned_denylist_is_rejected() {
        let dl = Denylist {
            schema: SCHEMA.to_string(),
            name: "x".into(),
            issuer_pubkey: String::new(),
            updated: "2026-06-24T00:00:00Z".into(),
            entries: vec![],
            key_entries: vec![],
            signature: None,
        };
        assert!(dl.verify().is_err());
    }

    #[test]
    fn legacy_v1_json_blob_fails_at_parse() {
        // Un vrai blob v1 (sans `key_entries`) n'atteint jamais verify() : le
        // champ est désormais obligatoire, le parsing JSON échoue en amont —
        // même politique zéro-compat que pour le feed (`champinium-feed/v3`).
        let legacy = r#"{"schema":"champinium-denylist/v1","name":"x","issuer_pubkey":"AAAA","updated":"2026-06-24T00:00:00Z","entries":["bafkreig"],"signature":"AAAA"}"#;
        assert!(Denylist::from_json(legacy).is_err());
    }

    #[test]
    fn oversized_denylist_is_rejected() {
        let issuer = Keypair::generate_ed25519();
        // Construit directement (sans passer par build_signed pour éviter de
        // signer 65 537 CIDs) une liste au-delà de la borne, puis la signe.
        let mut dl = Denylist {
            schema: SCHEMA.to_string(),
            name: "trop grande".into(),
            issuer_pubkey: B64.encode(issuer.public().encode_protobuf()),
            updated: "2026-07-23T00:00:00Z".into(),
            entries: (0..=MAX_DENYLIST_ENTRIES)
                .map(|i| cid_for(i.to_string().as_bytes()).to_string())
                .collect(),
            key_entries: vec![],
            signature: None,
        };
        let sig = issuer.sign(&dl.signing_bytes()).unwrap();
        dl.signature = Some(B64.encode(sig));

        assert!(dl.verify().is_err(), "la borne anti-abus doit rejeter");
        let mut m = Moderation::empty();
        assert!(m.subscribe(&dl).is_err());
    }

    #[test]
    fn is_blocked_key_false_when_not_subscribed() {
        let m = Moderation::empty();
        let peer = PeerId::from(Keypair::generate_ed25519().public());
        assert!(!m.is_blocked_key(&peer));
    }
}
