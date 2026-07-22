//! Feed mutable signé d'un créateur.
//!
//! Un feed est la liste, **signée par l'identité du créateur** (Ed25519), des CIDs
//! qu'il publie, ainsi que l'identité éditoriale de son channel (nom, description,
//! avatar). Il est **versionné** par un `seq` monotone : à chaque mise à jour, le
//! créateur réémet un feed avec un `seq` plus grand. La résolution de conflit est
//! « le plus grand `seq` gagne » (last-writer-wins), la signature garantissant
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

/// Identifiant de schéma de feed v3 : le feed porte l'identité éditoriale du
/// channel (nom, description, avatar) EN PLUS des métadonnées par contenu.
/// Formats v1/v2 supprimés (décision de spec channels, zéro utilisateur).
pub const SCHEMA: &str = "champinium-feed/v3";

/// Bornes des métadonnées de channel (anti-abus : le feed n'est pas un canal de
/// données arbitraires ; ces bornes sont VÉRIFIÉES à la réception, pas seulement
/// à la construction).
pub const MAX_CHANNEL_NAME_LEN: usize = 64;
pub const MAX_CHANNEL_DESC_LEN: usize = 1024;

/// Bornes des métadonnées par contenu (anti-abus : idem, vérifiées à la réception).
pub const MAX_TITLE_LEN: usize = 256;
pub const MAX_TAG_LEN: usize = 64;
pub const MAX_TAGS_PER_ENTRY: usize = 16;

/// Identité éditoriale d'un channel, signée avec le feed. L'avatar est un CID
/// d'image — modéré comme tout contenu (checkpoints inchangés).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelMeta {
    pub name: String,
    pub description: String,
    pub avatar_cid: Option<String>,
}

/// Un contenu publié dans un feed : son CID et ses métadonnées signées.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedEntry {
    /// CID du contenu (chaîne CIDv1) — pour une vidéo, le manifeste HLS.
    pub cid: String,
    /// Titre lisible (peut être vide).
    pub title: String,
    /// Tags normalisés (minuscules, sans espaces de bord).
    pub tags: Vec<String>,
}

/// Feed signé d'un créateur (format unique `champinium-feed/v3`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feed {
    /// Identifiant de schéma ; toujours [`SCHEMA`].
    pub schema: String,
    /// Clé publique Ed25519 de l'émetteur (protobuf libp2p, base64).
    pub issuer_pubkey: String,
    /// Numéro de version monotone (le plus grand gagne).
    pub seq: u64,
    /// Identité éditoriale du channel — signée avec le reste du feed.
    pub channel: ChannelMeta,
    /// Contenus publiés avec métadonnées.
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
    /// décalage de frontière. Le bloc channel est couvert, puis les entrées,
    /// triées par CID, chacune couvrant cid, titre et tags.
    fn signing_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        push_field(&mut buf, self.schema.as_bytes());
        push_field(&mut buf, self.issuer_pubkey.as_bytes());
        push_field(&mut buf, &self.seq.to_le_bytes());
        push_field(&mut buf, self.channel.name.as_bytes());
        push_field(&mut buf, self.channel.description.as_bytes());
        push_field(
            &mut buf,
            self.channel.avatar_cid.as_deref().unwrap_or("").as_bytes(),
        );
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
        buf
    }

    /// Construit et **signe** un feed sans métadonnées de contenu (titres vides)
    /// ni profil de channel. API de commodité pour les publications par CIDs nus.
    pub fn build_signed(issuer: &Keypair, seq: u64, cids: &[Cid]) -> CoreResult<Self> {
        let entries: Vec<FeedEntry> = cids
            .iter()
            .map(|c| FeedEntry {
                cid: c.to_string(),
                title: String::new(),
                tags: Vec::new(),
            })
            .collect();
        Self::build_signed_with(issuer, seq, &ChannelMeta::default(), &entries)
    }

    /// Construit et **signe** un feed avec profil de channel et métadonnées par
    /// contenu. Les tags sont normalisés (minuscules, sans espaces de bord ; les
    /// vides disparaissent).
    pub fn build_signed_with(
        issuer: &Keypair,
        seq: u64,
        channel: &ChannelMeta,
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
            schema: SCHEMA.to_string(),
            issuer_pubkey: B64.encode(issuer.public().encode_protobuf()),
            seq,
            channel: channel.clone(),
            entries,
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

    /// Vérifie le schéma, les **bornes** (channel + entries) et la **signature**
    /// du feed.
    pub fn verify(&self) -> CoreResult<()> {
        if self.schema != SCHEMA {
            return Err(CoreError::Network(format!(
                "schéma feed inconnu: {}",
                self.schema
            )));
        }
        if self.channel.name.len() > MAX_CHANNEL_NAME_LEN
            || self.channel.description.len() > MAX_CHANNEL_DESC_LEN
        {
            return Err(CoreError::Network(
                "métadonnées de channel hors bornes".into(),
            ));
        }
        if let Some(avatar) = &self.channel.avatar_cid {
            avatar
                .parse::<Cid>()
                .map_err(|_| CoreError::Network("avatar_cid invalide".into()))?;
        }
        for e in &self.entries {
            if e.title.len() > MAX_TITLE_LEN
                || e.tags.len() > MAX_TAGS_PER_ENTRY
                || e.tags.iter().any(|t| t.len() > MAX_TAG_LEN)
            {
                return Err(CoreError::Network("métadonnées de feed hors bornes".into()));
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

    /// CIDs du feed (après parsing), depuis `entries`.
    pub fn cids(&self) -> CoreResult<Vec<Cid>> {
        self.entries
            .iter()
            .map(|e| e.cid.parse::<Cid>().map_err(CoreError::Cid))
            .collect()
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
        feed.entries.push(entry(cid_for(b"injecte"), "", &[]));
        assert!(feed.verify().is_err());
    }

    #[test]
    fn bumping_seq_requires_resigning() {
        let issuer = Keypair::generate_ed25519();
        let mut feed = Feed::build_signed(&issuer, 1, &[cid_for(b"a")]).unwrap();
        feed.seq = 2; // modifie le seq sans re-signer
        assert!(feed.verify().is_err());
    }

    // --- métadonnées de contenu (titre, tags) signées ---

    fn entry(cid: Cid, title: &str, tags: &[&str]) -> FeedEntry {
        FeedEntry {
            cid: cid.to_string(),
            title: title.to_string(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
        }
    }

    fn channel(name: &str, desc: &str, avatar: Option<&str>) -> ChannelMeta {
        ChannelMeta {
            name: name.to_string(),
            description: desc.to_string(),
            avatar_cid: avatar.map(str::to_string),
        }
    }

    #[test]
    fn v2_roundtrips_with_metadata() {
        let issuer = Keypair::generate_ed25519();
        let feed = Feed::build_signed_with(
            &issuer,
            1,
            &ChannelMeta::default(),
            &[entry(
                cid_for(b"a"),
                "Aurores boréales",
                &["nature", "nuit"],
            )],
        )
        .unwrap();
        assert_eq!(feed.schema, SCHEMA);

        let parsed = Feed::from_json(feed.to_json().unwrap().as_bytes()).unwrap();
        parsed.verify().expect("signature valide");
        assert_eq!(parsed.cids().unwrap(), vec![cid_for(b"a")]);
        assert_eq!(parsed.entries[0].title, "Aurores boréales");
        assert_eq!(parsed.entries[0].tags, vec!["nature", "nuit"]);
    }

    #[test]
    fn v2_metadata_is_covered_by_signature() {
        let issuer = Keypair::generate_ed25519();
        let mut feed = Feed::build_signed_with(
            &issuer,
            1,
            &ChannelMeta::default(),
            &[entry(cid_for(b"a"), "Titre", &["tag"])],
        )
        .unwrap();
        feed.entries[0].title = "Titre falsifié".into();
        assert!(feed.verify().is_err(), "le titre est signé");

        let mut feed2 = Feed::build_signed_with(
            &issuer,
            1,
            &ChannelMeta::default(),
            &[entry(cid_for(b"a"), "Titre", &["tag"])],
        )
        .unwrap();
        feed2.entries[0].tags.push("injecte".into());
        assert!(feed2.verify().is_err(), "les tags sont signés");
    }

    #[test]
    fn v3_carries_signed_channel_metadata() {
        let issuer = Keypair::generate_ed25519();
        let feed = Feed::build_signed_with(
            &issuer,
            1,
            &channel(
                "Aurores",
                "Vidéos IA de ciels nocturnes",
                Some(&cid_for(b"avatar").to_string()),
            ),
            &[entry(cid_for(b"a"), "Nuit 1", &["nature"])],
        )
        .unwrap();
        assert_eq!(feed.schema, SCHEMA);

        let parsed = Feed::from_json(feed.to_json().unwrap().as_bytes()).unwrap();
        parsed.verify().unwrap();
        assert_eq!(parsed.channel.name, "Aurores");
        assert_eq!(
            parsed.channel.avatar_cid,
            Some(cid_for(b"avatar").to_string())
        );

        // Le bloc channel est COUVERT par la signature.
        let mut forged = parsed.clone();
        forged.channel.name = "Usurpé".into();
        assert!(forged.verify().is_err(), "le nom de channel est signé");
    }

    #[test]
    fn v3_rejects_out_of_bounds_channel_metadata() {
        let issuer = Keypair::generate_ed25519();
        let long_name = "n".repeat(MAX_CHANNEL_NAME_LEN + 1);
        let feed =
            Feed::build_signed_with(&issuer, 1, &channel(&long_name, "", None), &[]).unwrap();
        assert!(feed.verify().is_err());

        let long_desc = "d".repeat(MAX_CHANNEL_DESC_LEN + 1);
        let feed =
            Feed::build_signed_with(&issuer, 1, &channel("n", &long_desc, None), &[]).unwrap();
        assert!(feed.verify().is_err());

        // L'avatar, s'il est présent, doit être un CID valide.
        let feed = Feed::build_signed_with(&issuer, 1, &channel("n", "", Some("pas-un-cid")), &[])
            .unwrap();
        assert!(feed.verify().is_err());
    }

    #[test]
    fn legacy_schemas_are_rejected() {
        // Décision de spec (zéro utilisateur) : v1/v2 supprimés, pas dépréciés.
        let issuer = Keypair::generate_ed25519();
        let mut feed = Feed::build_signed(&issuer, 1, &[cid_for(b"a")]).unwrap();
        feed.schema = "champinium-feed/v2".into();
        assert!(feed.verify().is_err(), "schéma inconnu → rejet");
    }

    #[test]
    fn v2_rejects_out_of_bounds_metadata() {
        let issuer = Keypair::generate_ed25519();
        // Titre trop long.
        let long_title = "t".repeat(MAX_TITLE_LEN + 1);
        let feed = Feed::build_signed_with(
            &issuer,
            1,
            &ChannelMeta::default(),
            &[entry(cid_for(b"a"), &long_title, &[])],
        )
        .unwrap();
        assert!(feed.verify().is_err());

        // Trop de tags.
        let many: Vec<String> = (0..MAX_TAGS_PER_ENTRY + 1)
            .map(|i| format!("tag{i}"))
            .collect();
        let many_refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let feed = Feed::build_signed_with(
            &issuer,
            1,
            &ChannelMeta::default(),
            &[entry(cid_for(b"a"), "t", &many_refs)],
        )
        .unwrap();
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
            &ChannelMeta::default(),
            &[entry(cid_for(b"a"), "T", &["  NaTure ", "", "nuit"])],
        )
        .unwrap();
        feed.verify().unwrap();
        assert_eq!(feed.entries[0].tags, vec!["nature", "nuit"]);
    }

    #[test]
    fn legacy_json_blobs_fail_at_parse() {
        // Un vrai feed v1/v2 sérialisé n'atteint jamais verify() : le champ
        // obligatoire `channel` manque, le parsing JSON échoue en amont.
        let legacy = r#"{"schema":"champinium-feed/v1","issuer_pubkey":"AAAA","seq":1,"cids":["bafkreig"],"signature":"AAAA"}"#;
        assert!(Feed::from_json(legacy.as_bytes()).is_err());
    }
}
