//! Moteur de modération — garde-fou OBLIGATOIRE, actif par défaut.
//!
//! Sur un réseau décentralisé, la suppression centrale est impossible : la
//! modération est donc côté nœud, via des **denylists signées souscrites**
//! (modèle fédéré) : objets signés Ed25519 qu'un nœud choisit de suivre ; leur
//! signature est **vérifiée** avant prise en compte. Format
//! `champinium-denylist/v3` — v2/v1 sont supprimés : politique zéro-compat déjà
//! appliquée aux feeds (`champinium-feed/v3`).
//!
//! Le binaire embarque uniquement la **clé** de l'éditeur de la liste projet
//! ([`PROJECT_ISSUER`] / [`project_issuer`]), pas la liste elle-même : la liste
//! signée est récupérée sur le réseau (DHT) et peut être mise à jour sans
//! nouvelle release (spec 2026-09-06, ADR 0011).
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
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Identifiant de schéma de denylist.
pub const SCHEMA: &str = "champinium-denylist/v3";

/// Nombre maximal d'entrées (CIDs + clés cumulés) dans une denylist — borne
/// anti-abus, absente en v1, posée avec l'ajout des entrées de clés.
pub const MAX_DENYLIST_ENTRIES: usize = 65_536;

/// Taille max d'une liste (JSON, octets) — record DHT et parsing.
pub const MAX_DENYLIST_SIZE: usize = 1_048_576;

/// PeerId de l'éditeur de la LISTE PROJET, compilé dans le binaire (spec
/// 2026-09-06, ADR 0011). Le binaire embarque la clé, pas la liste : la liste
/// signée est récupérée dans la DHT et mise à jour sans release.
pub const PROJECT_ISSUER: &str = include_str!("../../../deny/project.issuer");

/// Parse [`PROJECT_ISSUER`] (lignes vides et `#` ignorées). Une valeur
/// invalide est une erreur de build : `Node::open` échoue.
pub fn project_issuer() -> CoreResult<PeerId> {
    let line = PROJECT_ISSUER
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .ok_or_else(|| CoreError::Moderation("deny/project.issuer vide".into()))?;
    PeerId::from_str(line)
        .map_err(|e| CoreError::Moderation(format!("deny/project.issuer invalide: {e}")))
}

/// Denylist signée souscrite (format `champinium-denylist/v3`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Denylist {
    /// Identifiant de schéma ; doit valoir [`SCHEMA`].
    pub schema: String,
    /// Nom lisible de la liste.
    pub name: String,
    /// Clé publique Ed25519 de l'émetteur (protobuf libp2p, encodé base64).
    pub issuer_pubkey: String,
    /// Numéro de séquence signé : permet de rejeter un rejeu d'une version
    /// antérieure de la liste par un tiers qui la relaierait.
    pub seq: u64,
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
        push_field(&mut buf, &self.seq.to_le_bytes());
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
        seq: u64,
        entries: &[Cid],
        keys: &[PeerId],
    ) -> CoreResult<Self> {
        let mut dl = Self {
            schema: SCHEMA.to_string(),
            name: name.to_string(),
            issuer_pubkey: B64.encode(issuer.public().encode_protobuf()),
            seq,
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

    /// Parse une denylist depuis du JSON. Refuse tout document dépassant
    /// [`MAX_DENYLIST_SIZE`] octets avant même de tenter le parsing (borne
    /// anti-abus sur un record potentiellement reçu depuis la DHT).
    pub fn from_json(json: &str) -> CoreResult<Self> {
        if json.len() > MAX_DENYLIST_SIZE {
            return Err(CoreError::Moderation("denylist trop volumineuse".into()));
        }
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

    /// PeerId de l'émetteur, dérivé de `issuer_pubkey`.
    pub fn issuer_peer_id(&self) -> CoreResult<PeerId> {
        let pk_bytes = B64
            .decode(&self.issuer_pubkey)
            .map_err(|e| CoreError::Moderation(format!("clé base64: {e}")))?;
        let pk = PublicKey::try_decode_protobuf(&pk_bytes)
            .map_err(|e| CoreError::Moderation(format!("clé invalide: {e}")))?;
        Ok(pk.to_peer_id())
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

/// Résultat de [`Moderation::apply_list`] : soit la liste était plus récente
/// que ce qui est connu de cet éditeur (et remplace ses entrées), soit elle
/// est périmée (`seq` inférieur ou égal au `seq` connu) et est ignorée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// La liste a remplacé les entrées connues de cet éditeur.
    Newer {
        /// Nombre de CIDs désormais bloqués pour cet éditeur.
        added_cids: usize,
        /// Nombre de clés désormais bloquées pour cet éditeur.
        added_keys: usize,
    },
    /// La liste est périmée (`seq` ≤ `seq` connu) : ignorée.
    Stale,
}

/// Instantané des entrées connues pour un éditeur de denylist donné.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenylistSource {
    /// Éditeur de la liste.
    pub issuer: PeerId,
    /// Nom lisible de la liste.
    pub name: String,
    /// Numéro de séquence courant.
    pub seq: u64,
    /// Horodatage de mise à jour.
    pub updated: String,
    /// Nombre de CIDs bloqués par cet éditeur.
    pub entry_count: usize,
    /// Nombre de clés bloquées par cet éditeur.
    pub key_count: usize,
}

/// Entrées connues pour un éditeur donné (partie de l'index par éditeur).
#[derive(Debug, Clone, Default)]
struct IssuerEntries {
    name: String,
    seq: u64,
    updated: String,
    cids: HashSet<Cid>,
    keys: HashSet<PeerId>,
}

/// Moteur de modération : entrées **par éditeur** (souscription à des
/// denylists signées — voir le doc de module) + vues agrégées reconstruites à
/// chaque changement, pour garder le chemin chaud des checkpoints
/// (`is_blocked`/`is_blocked_key`) en O(1) et sans changement de signature.
#[derive(Debug, Clone, Default)]
pub struct Moderation {
    by_issuer: std::collections::HashMap<PeerId, IssuerEntries>,
    blocked: HashSet<Cid>,
    blocked_keys: HashSet<PeerId>,
}

impl Moderation {
    /// Moteur vide (aucune souscription).
    pub fn new() -> Self {
        Self::default()
    }

    /// Alias de [`Moderation::new`] (nom historique, encore utilisé par les tests).
    pub fn empty() -> Self {
        Self::default()
    }

    fn rebuild_views(&mut self) {
        self.blocked = self
            .by_issuer
            .values()
            .flat_map(|e| e.cids.iter().copied())
            .collect();
        self.blocked_keys = self
            .by_issuer
            .values()
            .flat_map(|e| e.keys.iter().copied())
            .collect();
    }

    /// Applique une denylist signée : **vérifie la signature**, puis remplace
    /// les entrées connues de son éditeur si `seq` est strictement supérieur
    /// au `seq` connu (une liste périmée, `seq` ≤ connu, est ignorée — protège
    /// contre le rejeu d'une version antérieure par un tiers qui la
    /// relaierait).
    pub fn apply_list(&mut self, list: &Denylist) -> CoreResult<Applied> {
        list.verify()?;
        let issuer = list.issuer_peer_id()?;
        if let Some(known) = self.by_issuer.get(&issuer) {
            if list.seq <= known.seq {
                return Ok(Applied::Stale);
            }
        }
        let cids = list.cids()?;
        let keys = list.keys()?;
        let applied = Applied::Newer {
            added_cids: cids.len(),
            added_keys: keys.len(),
        };
        self.by_issuer.insert(
            issuer,
            IssuerEntries {
                name: list.name.clone(),
                seq: list.seq,
                updated: list.updated.clone(),
                cids,
                keys,
            },
        );
        self.rebuild_views();
        Ok(applied)
    }

    /// Souscrit à une denylist signée (conservé pour compatibilité : mince
    /// enveloppe autour de [`Moderation::apply_list`]).
    pub fn subscribe(&mut self, list: &Denylist) -> CoreResult<()> {
        self.apply_list(list).map(|_| ())
    }

    /// Retire toutes les entrées connues d'un éditeur. Renvoie `true` si
    /// l'éditeur était connu (et a donc été retiré).
    pub fn remove_issuer(&mut self, issuer: &PeerId) -> bool {
        let removed = self.by_issuer.remove(issuer).is_some();
        if removed {
            self.rebuild_views();
        }
        removed
    }

    /// `seq` connu pour un éditeur donné, s'il est souscrit.
    pub fn known_seq(&self, issuer: &PeerId) -> Option<u64> {
        self.by_issuer.get(issuer).map(|e| e.seq)
    }

    /// Instantané des entrées connues pour un éditeur donné.
    pub fn source(&self, issuer: &PeerId) -> Option<DenylistSource> {
        self.by_issuer.get(issuer).map(|e| DenylistSource {
            issuer: *issuer,
            name: e.name.clone(),
            seq: e.seq,
            updated: e.updated.clone(),
            entry_count: e.cids.len(),
            key_count: e.keys.len(),
        })
    }

    /// Indique si un CID est bloqué (vue agrégée, tous éditeurs confondus).
    pub fn is_blocked(&self, cid: &Cid) -> bool {
        self.blocked.contains(cid)
    }

    /// Indique si une clé (PeerId) est bloquée en entier (vue agrégée, tous
    /// éditeurs confondus).
    pub fn is_blocked_key(&self, peer: &PeerId) -> bool {
        self.blocked_keys.contains(peer)
    }

    /// Nombre de CIDs bloqués (vue agrégée).
    pub fn len(&self) -> usize {
        self.blocked.len()
    }

    /// Vrai si aucun CID n'est bloqué (vue agrégée).
    pub fn is_empty(&self) -> bool {
        self.blocked.is_empty()
    }
}

/// Répertoire de cache disque des denylists souscrites (`root/.denylists`).
pub fn denylist_cache_dir(root: &Path) -> PathBuf {
    root.join(".denylists")
}

/// Persiste une denylist signée dans le cache disque, sous
/// `<root>/.denylists/<peerid>.json`.
///
/// Écriture atomique (fichier temporaire puis rename, même patron que
/// [`crate::blockstore::Blockstore::put`], minor M4 de la revue finale
/// 2026-09-06) : une coupure en cours d'écriture ne laisse jamais de fichier
/// tronqué qui serait silencieusement ignoré — et donc perdrait la protection
/// hors ligne de cet éditeur — au prochain démarrage.
pub fn save_cached_list(root: &Path, list: &Denylist) -> CoreResult<()> {
    let dir = denylist_cache_dir(root);
    std::fs::create_dir_all(&dir)?;
    let json =
        serde_json::to_string(list).map_err(|e| CoreError::Moderation(format!("json: {e}")))?;
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    std::io::Write::write_all(&mut tmp, json.as_bytes())?;
    tmp.persist(dir.join(format!("{}.json", list.issuer_peer_id()?)))
        .map_err(|e| CoreError::Io(e.error))?;
    Ok(())
}

/// Charge toutes les denylists mises en cache. Un fichier illisible ou
/// invalide (JSON corrompu, signature invalide) est **ignoré** — journalisé
/// via `tracing::warn!` — plutôt que de faire échouer le chargement complet.
pub fn load_cached_lists(root: &Path) -> Vec<Denylist> {
    let Ok(entries) = std::fs::read_dir(denylist_cache_dir(root)) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            match Denylist::from_json(&text).and_then(|l| l.verify().map(|_| l)) {
                Ok(l) => Some(l),
                Err(err) => {
                    tracing::warn!("cache de denylist ignoré ({}): {err}", e.path().display());
                    None
                }
            }
        })
        .collect()
}

/// Supprime la denylist mise en cache pour un éditeur donné, si elle existe.
pub fn remove_cached_list(root: &Path, issuer: &PeerId) {
    let _ = std::fs::remove_file(denylist_cache_dir(root).join(format!("{issuer}.json")));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::cid_for;

    #[test]
    fn seq_is_signed_and_v2_is_rejected() {
        let issuer = Keypair::generate_ed25519();
        let dl = Denylist::build_signed("t", "2026-09-06T00:00:00Z", &issuer, 7, &[], &[]).unwrap();
        dl.verify().unwrap();
        assert_eq!(dl.seq, 7);
        let mut tampered = dl.clone();
        tampered.seq = 8;
        assert!(
            tampered.verify().is_err(),
            "changer seq invalide la signature"
        );
        let mut v2 = dl.clone();
        v2.schema = "champinium-denylist/v2".into();
        assert!(v2.verify().is_err());
        let json = serde_json::to_string(&dl)
            .unwrap()
            .replace(r#""seq":7,"#, "");
        assert!(
            Denylist::from_json(&json).is_err(),
            "seq obligatoire au parsing"
        );
    }

    #[test]
    fn oversized_json_is_rejected_before_parsing() {
        let big = format!(
            r#"{{"schema":"x","pad":"{}"}}"#,
            "a".repeat(MAX_DENYLIST_SIZE)
        );
        assert!(Denylist::from_json(&big).is_err());
    }

    #[test]
    fn project_issuer_parses() {
        let p = project_issuer().expect("deny/project.issuer doit contenir un PeerId valide");
        assert!(!p.to_string().is_empty());
    }

    #[test]
    fn issuer_peer_id_matches_keypair() {
        let issuer = Keypair::generate_ed25519();
        let dl = Denylist::build_signed("t", "2026-09-06T00:00:00Z", &issuer, 1, &[], &[]).unwrap();
        assert_eq!(dl.issuer_peer_id().unwrap(), issuer.public().to_peer_id());
    }

    #[test]
    fn signed_denylist_roundtrips_and_blocks() {
        let issuer = Keypair::generate_ed25519();
        let bad = cid_for(b"contenu interdit");
        let dl = Denylist::build_signed("test", "2026-06-24T00:00:00Z", &issuer, 1, &[bad], &[])
            .unwrap();

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
        let dl = Denylist::build_signed(
            "test",
            "2026-07-23T00:00:00Z",
            &issuer,
            1,
            &[],
            &[banned_peer],
        )
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
            Denylist::build_signed("t", "2026-07-23T00:00:00Z", &issuer, 1, &[], &[]).unwrap();
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
        let mut dl = Denylist::build_signed(
            "t",
            "2026-06-24T00:00:00Z",
            &issuer,
            1,
            &[cid_for(b"x")],
            &[],
        )
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
        let legit = Denylist::build_signed("n", "u", &issuer, 1, &[a, b], &[]).unwrap();

        // Attaque : on déplace le premier CID depuis `entries` vers `updated`.
        // Avec une concaténation naïve séparée par '\n', les octets signés sont
        // identiques → la même signature validerait cette liste falsifiée.
        let forged = Denylist {
            schema: legit.schema.clone(),
            name: legit.name.clone(),
            issuer_pubkey: legit.issuer_pubkey.clone(),
            updated: format!("u\n{a}"),
            seq: legit.seq,
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
        let legit = Denylist::build_signed("n", "u", &issuer, 1, &[], &[peer]).unwrap();

        let forged = Denylist {
            schema: legit.schema.clone(),
            name: legit.name.clone(),
            issuer_pubkey: legit.issuer_pubkey.clone(),
            updated: legit.updated.clone(),
            seq: legit.seq,
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
        let mut dl = Denylist::build_signed(
            "t",
            "2026-06-24T00:00:00Z",
            &issuer,
            1,
            &[cid_for(b"y")],
            &[],
        )
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
            seq: 1,
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
            seq: 1,
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

    fn list(issuer: &Keypair, seq: u64, cids: &[Cid], keys: &[PeerId]) -> Denylist {
        Denylist::build_signed("l", "2026-09-06T00:00:00Z", issuer, seq, cids, keys).unwrap()
    }

    #[test]
    fn apply_list_replaces_per_issuer_and_ignores_stale() {
        let a = Keypair::generate_ed25519();
        let c1 = cid_for(b"1");
        let c2 = cid_for(b"2");
        let mut m = Moderation::new();
        assert!(matches!(
            m.apply_list(&list(&a, 1, &[c1], &[])).unwrap(),
            Applied::Newer { added_cids: 1, .. }
        ));
        assert!(m.is_blocked(&c1));
        // seq 2 remplace : c1 sort, c2 entre.
        assert!(matches!(
            m.apply_list(&list(&a, 2, &[c2], &[])).unwrap(),
            Applied::Newer { .. }
        ));
        assert!(!m.is_blocked(&c1) && m.is_blocked(&c2));
        // seq 1 (périmé) ignoré.
        assert!(matches!(
            m.apply_list(&list(&a, 1, &[c1], &[])).unwrap(),
            Applied::Stale
        ));
        assert!(!m.is_blocked(&c1));
        let s = m.source(&a.public().to_peer_id()).unwrap();
        assert_eq!((s.seq, s.entry_count, s.key_count), (2, 1, 0));
    }

    #[test]
    fn entry_shared_by_two_issuers_survives_one_removal() {
        let a = Keypair::generate_ed25519();
        let b = Keypair::generate_ed25519();
        let bad = PeerId::from(Keypair::generate_ed25519().public());
        let mut m = Moderation::new();
        m.apply_list(&list(&a, 1, &[], &[bad])).unwrap();
        m.apply_list(&list(&b, 1, &[], &[bad])).unwrap();
        assert!(m.remove_issuer(&a.public().to_peer_id()));
        assert!(m.is_blocked_key(&bad), "encore portée par b");
        assert!(m.remove_issuer(&b.public().to_peer_id()));
        assert!(!m.is_blocked_key(&bad));
        assert!(!m.remove_issuer(&a.public().to_peer_id()), "déjà retiré");
    }

    #[test]
    fn cache_roundtrip_and_corrupt_file_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let a = Keypair::generate_ed25519();
        let l = list(&a, 3, &[cid_for(b"x")], &[]);
        save_cached_list(dir.path(), &l).unwrap();
        std::fs::write(
            denylist_cache_dir(dir.path()).join("corrompu.json"),
            b"{not json",
        )
        .unwrap();
        let loaded = load_cached_lists(dir.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].seq, 3);
        remove_cached_list(dir.path(), &a.public().to_peer_id());
        assert!(load_cached_lists(dir.path()).is_empty());
    }
}
