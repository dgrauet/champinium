//! Construction et signature d'une transaction Arweave (format 2), à la main —
//! aucune dépendance Arweave (décision supply-chain, cf.
//! `.superpowers/sdd/coldstore-decision.md`). Logique pure, sans réseau : le
//! backend [`super::arweave`] s'en sert pour signer puis POSTer.
//!
//! Trois primitives cryptographiques, chacune conforme à l'implémentation de
//! référence `arweave-js` (ArweaveTeam/arweave-js) :
//!
//! - **deep hash** (`deepHash.ts`) : hachage récursif d'une structure
//!   arborescente (blobs et listes), **SHA-384** de bout en bout. Sert à
//!   produire les octets à signer à partir des champs de la transaction.
//! - **data_root** (`merkle.ts`) : racine de Merkle des chunks de données,
//!   **SHA-256**, avec la règle de rééquilibrage du dernier chunk. Champ
//!   `data_root` de la transaction format 2.
//! - **signature** : **RSA-PSS** (hash SHA-256, longueur de sel = 32 =
//!   taille du digest), signeur *aveuglé* (`BlindedSigningKey`, mitigation
//!   Marvin). L'identifiant de transaction est `SHA-256(signature)`.
//!
//! Ces trois algorithmes ne peuvent pas être validés contre un vrai nœud
//! Arweave en CI : ils sont épinglés par des vecteurs de test indépendants
//! (voir le module `tests` en bas) calculés par une implémentation de
//! référence séparée (transcrite depuis `arweave-js` et exécutée en Python),
//! jamais par auto-référence.

use crate::error::{CoreError, Result as CoreResult};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rsa::pss::BlindedSigningKey;
use rsa::signature::{RandomizedSigner, SignatureEncoding};
use rsa::{BoxedUint, RsaPrivateKey};
use sha2::{Digest, Sha256, Sha384};

/// Taille maximale d'un chunk Arweave (256 Kio).
const MAX_CHUNK_SIZE: usize = 256 * 1024;
/// Taille minimale d'un chunk avant rééquilibrage du dernier (32 Kio).
const MIN_CHUNK_SIZE: usize = 32 * 1024;

// --- deep hash (SHA-384) -------------------------------------------------

/// Nœud d'entrée de l'algorithme deep hash : soit une feuille d'octets
/// (`Blob`), soit une liste de nœuds. On emprunte les octets (`&[u8]`) pour ne
/// jamais recopier les données volumineuses d'une publication.
pub(crate) enum DeepHashItem<'a> {
    Blob(&'a [u8]),
    List(Vec<DeepHashItem<'a>>),
}

/// SHA-384 d'un tampon, en tableau de 48 octets.
fn sha384(data: &[u8]) -> [u8; 48] {
    let mut out = [0u8; 48];
    out.copy_from_slice(&Sha384::digest(data));
    out
}

/// Deep hash d'Arweave (`arweave-js` `deepHash.ts`). Un blob de longueur `n`
/// donne `SHA-384( SHA-384("blob" || n) || SHA-384(data) )` ; une liste de `k`
/// éléments part de l'accumulateur `SHA-384("list" || k)` et le replie à
/// gauche : `acc := SHA-384(acc || deep_hash(elem))` pour chaque élément.
pub(crate) fn deep_hash(item: &DeepHashItem) -> [u8; 48] {
    match item {
        DeepHashItem::Blob(data) => {
            let mut tag = Vec::with_capacity(4 + 20);
            tag.extend_from_slice(b"blob");
            tag.extend_from_slice(data.len().to_string().as_bytes());
            let mut h = Sha384::new();
            h.update(sha384(&tag));
            h.update(sha384(data));
            let mut out = [0u8; 48];
            out.copy_from_slice(&h.finalize());
            out
        }
        DeepHashItem::List(items) => {
            let mut tag = Vec::with_capacity(4 + 20);
            tag.extend_from_slice(b"list");
            tag.extend_from_slice(items.len().to_string().as_bytes());
            let mut acc = sha384(&tag);
            for it in items {
                let child = deep_hash(it);
                let mut h = Sha384::new();
                h.update(acc);
                h.update(child);
                acc.copy_from_slice(&h.finalize());
            }
            acc
        }
    }
}

// --- data_root : racine de Merkle des chunks (SHA-256) -------------------

/// SHA-256 d'un tampon, en tableau de 32 octets.
fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(data));
    out
}

/// Encode un entier en tampon big-endian de 32 octets (`noteToBuffer`
/// d'Arweave : rempli de droite à gauche).
fn note_to_buffer(mut n: usize) -> [u8; 32] {
    let mut buf = [0u8; 32];
    for slot in buf.iter_mut().rev() {
        *slot = (n % 256) as u8;
        n /= 256;
    }
    buf
}

/// Un chunk : son hash de données et sa borne haute d'octet (offset cumulé).
struct Chunk {
    data_hash: [u8; 32],
    max_byte_range: usize,
}

/// Un nœud de l'arbre de Merkle : identifiant + borne haute d'octet.
#[derive(Clone)]
struct MerkleNode {
    id: [u8; 32],
    max_byte_range: usize,
}

/// Découpe `data` en chunks de 256 Kio, avec la règle de rééquilibrage
/// d'Arweave : si le reliquat après un chunk plein serait plus petit que
/// 32 Kio, on partage le reste en deux moitiés (`ceil(reste / 2)`) plutôt que
/// de laisser un dernier chunk minuscule.
fn chunk_data(data: &[u8]) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut rest = data;
    let mut cursor = 0usize;
    while rest.len() >= MAX_CHUNK_SIZE {
        let mut chunk_size = MAX_CHUNK_SIZE;
        let next_size = rest.len() - MAX_CHUNK_SIZE;
        if next_size > 0 && next_size < MIN_CHUNK_SIZE {
            chunk_size = rest.len().div_ceil(2);
        }
        let (chunk, tail) = rest.split_at(chunk_size);
        cursor += chunk.len();
        chunks.push(Chunk {
            data_hash: sha256(chunk),
            max_byte_range: cursor,
        });
        rest = tail;
    }
    chunks.push(Chunk {
        data_hash: sha256(rest),
        max_byte_range: cursor + rest.len(),
    });
    chunks
}

/// Feuille de Merkle : `id = SHA-256( SHA-256(data_hash) ||
/// SHA-256(note(max_byte_range)) )`.
fn leaf(chunk: &Chunk) -> MerkleNode {
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&sha256(&chunk.data_hash));
    buf.extend_from_slice(&sha256(&note_to_buffer(chunk.max_byte_range)));
    MerkleNode {
        id: sha256(&buf),
        max_byte_range: chunk.max_byte_range,
    }
}

/// Réduit une couche de nœuds : hache chaque paire en un nœud de branche
/// `SHA-256( SHA-256(gauche.id) || SHA-256(droite.id) ||
/// SHA-256(note(gauche.max)) )` ; un nœud impair est promu tel quel.
fn build_layers(mut nodes: Vec<MerkleNode>) -> MerkleNode {
    while nodes.len() > 1 {
        let mut next = Vec::with_capacity(nodes.len().div_ceil(2));
        let mut i = 0;
        while i < nodes.len() {
            if i + 1 >= nodes.len() {
                next.push(nodes[i].clone());
                i += 1;
                continue;
            }
            let left = &nodes[i];
            let right = &nodes[i + 1];
            let mut buf = Vec::with_capacity(96);
            buf.extend_from_slice(&sha256(&left.id));
            buf.extend_from_slice(&sha256(&right.id));
            buf.extend_from_slice(&sha256(&note_to_buffer(left.max_byte_range)));
            next.push(MerkleNode {
                id: sha256(&buf),
                max_byte_range: right.max_byte_range,
            });
            i += 2;
        }
        nodes = next;
    }
    // `data` n'est jamais vide dans nos usages (manifeste/segment) → au moins
    // une feuille. On borne néanmoins pour ne pas paniquer.
    nodes
        .into_iter()
        .next()
        .map(|n| n.id)
        .map(|id| MerkleNode {
            id,
            max_byte_range: 0,
        })
        .unwrap_or(MerkleNode {
            id: [0u8; 32],
            max_byte_range: 0,
        })
}

/// Racine de Merkle des données (`data_root` d'une transaction format 2).
pub(crate) fn data_root(data: &[u8]) -> [u8; 32] {
    let chunks = chunk_data(data);
    let leaves: Vec<MerkleNode> = chunks.iter().map(leaf).collect();
    build_layers(leaves).id
}

// --- clé RSA depuis le JWK + signature RSA-PSS ---------------------------

/// Clé de portefeuille Arweave chargée depuis un JWK : la clé privée RSA et
/// les octets bruts du module public `n` (le champ `owner` de la transaction,
/// aussi la base de l'adresse du portefeuille).
pub(crate) struct WalletKey {
    private: RsaPrivateKey,
    owner: Vec<u8>,
}

/// Décode un champ base64url (sans padding) d'un JWK.
fn jwk_field(jwk: &serde_json::Value, name: &str) -> CoreResult<Vec<u8>> {
    let s = jwk
        .get(name)
        .and_then(|v| v.as_str())
        .ok_or_else(|| CoreError::Identity(format!("JWK Arweave: champ '{name}' absent")))?;
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| CoreError::Identity(format!("JWK Arweave: champ '{name}' non base64url: {e}")))
}

impl WalletKey {
    /// Reconstruit la clé privée RSA depuis les composants d'un JWK (`n`, `e`,
    /// `d`, `p`, `q`). On suit le même conditionnement de précision que
    /// `rsa`'s `TryFrom<pkcs1::RsaPrivateKey>` : les entiers `n`, `d`, `p`,
    /// `q` prennent la précision (en bits) du module ; `e` la sienne. `rsa`
    /// valide la cohérence (produit des premiers = module) au montage.
    pub(crate) fn from_jwk(jwk: &serde_json::Value) -> CoreResult<Self> {
        let n = jwk_field(jwk, "n")?;
        let e = jwk_field(jwk, "e")?;
        let d = jwk_field(jwk, "d")?;
        let p = jwk_field(jwk, "p")?;
        let q = jwk_field(jwk, "q")?;

        let n_bits = (n.len() as u32) * 8;
        let e_bits = (e.len() as u32) * 8;
        let uint = |bytes: &[u8], bits: u32, field: &str| -> CoreResult<BoxedUint> {
            BoxedUint::from_be_slice(bytes, bits).map_err(|e| {
                CoreError::Identity(format!("JWK Arweave: champ '{field}' invalide: {e:?}"))
            })
        };
        let n_u = uint(&n, n_bits, "n")?;
        let e_u = uint(&e, e_bits, "e")?;
        let d_u = uint(&d, n_bits, "d")?;
        let p_u = uint(&p, n_bits, "p")?;
        let q_u = uint(&q, n_bits, "q")?;

        let private = RsaPrivateKey::from_components(n_u, e_u, d_u, vec![p_u, q_u])
            .map_err(|e| CoreError::Identity(format!("JWK Arweave: clé RSA invalide: {e}")))?;
        Ok(Self { private, owner: n })
    }

    /// Octets bruts du module public `n` — le champ `owner` d'une transaction.
    pub(crate) fn owner(&self) -> &[u8] {
        &self.owner
    }

    /// Signe `message` en RSA-PSS (SHA-256, sel 32, signeur aveuglé) et renvoie
    /// les octets bruts de la signature. Aucun `unwrap` : l'échec de génération
    /// d'aléa ou de signature remonte en erreur.
    pub(crate) fn sign(&self, message: &[u8]) -> CoreResult<Vec<u8>> {
        let signing_key = BlindedSigningKey::<Sha256>::new(self.private.clone());
        let mut rng = getrandom::SysRng;
        let sig = signing_key
            .try_sign_with_rng(&mut rng, message)
            .map_err(|e| CoreError::Network(format!("signature RSA-PSS Arweave: {e}")))?;
        Ok(sig.to_bytes().into_vec())
    }
}

// --- transaction format 2 : octets à signer + corps JSON -----------------

/// Étiquette (tag) d'une transaction, en clair. Encodée base64url dans le JSON
/// posté ; ses octets UTF-8 bruts entrent dans le deep hash.
pub(crate) struct Tag {
    pub name: String,
    pub value: String,
}

/// Une transaction Arweave format 2 prête à signer/poster. Les champs
/// numériques (`quantity`, `reward`, `data_size`) sont des chaînes décimales,
/// conformément au format on-chain.
pub(crate) struct Transaction {
    owner: Vec<u8>,
    last_tx: Vec<u8>,
    reward: String,
    tags: Vec<Tag>,
    data: Vec<u8>,
    data_root: [u8; 32],
    signature: Vec<u8>,
    id: Vec<u8>,
}

impl Transaction {
    /// Construit et **signe** une transaction format 2 : `data` sans transfert
    /// d'AR (`quantity = 0`, pas de `target`), `last_tx` = ancre fournie par la
    /// gateway, `reward` = prix winston pour cette taille. Calcule `data_root`,
    /// dérive les octets à signer par deep hash, signe en RSA-PSS, puis
    /// `id = SHA-256(signature)`.
    pub(crate) fn build_signed(
        wallet: &WalletKey,
        data: Vec<u8>,
        tags: Vec<Tag>,
        last_tx: Vec<u8>,
        reward: String,
    ) -> CoreResult<Self> {
        let data_root = data_root(&data);
        let data_size = data.len().to_string();

        // Octets à signer : deep hash de l'ordre exact des champs format 2
        // (arweave-js `transaction.ts` getSignatureData). `owner`, `target`,
        // `last_tx`, `data_root` sont des octets bruts ; `format`, `quantity`,
        // `reward`, `data_size` des chaînes UTF-8 ; les tags une liste de
        // paires [nom, valeur] en octets UTF-8 bruts.
        let tag_items: Vec<DeepHashItem> = tags
            .iter()
            .map(|t| {
                DeepHashItem::List(vec![
                    DeepHashItem::Blob(t.name.as_bytes()),
                    DeepHashItem::Blob(t.value.as_bytes()),
                ])
            })
            .collect();
        let sig_data = deep_hash(&DeepHashItem::List(vec![
            DeepHashItem::Blob(b"2"),
            DeepHashItem::Blob(wallet.owner()),
            DeepHashItem::Blob(b""),  // target : aucun
            DeepHashItem::Blob(b"0"), // quantity : aucun transfert
            DeepHashItem::Blob(reward.as_bytes()),
            DeepHashItem::Blob(&last_tx),
            DeepHashItem::List(tag_items),
            DeepHashItem::Blob(data_size.as_bytes()),
            DeepHashItem::Blob(&data_root),
        ]));

        let signature = wallet.sign(&sig_data)?;
        let id = sha256(&signature).to_vec();

        Ok(Self {
            owner: wallet.owner().to_vec(),
            last_tx,
            reward,
            tags,
            data,
            data_root,
            signature,
            id,
        })
    }

    /// Identifiant de transaction (base64url de `SHA-256(signature)`).
    pub(crate) fn id_b64(&self) -> String {
        URL_SAFE_NO_PAD.encode(&self.id)
    }

    /// Corps JSON à POSTer sur `POST /tx` d'une gateway (données en ligne,
    /// base64url). Convient aux tailles acceptées en ligne par les gateways ;
    /// les très gros volumes exigeraient l'upload par chunks (`POST /chunk`),
    /// hors périmètre de cette tâche.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        let tags: Vec<serde_json::Value> = self
            .tags
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": URL_SAFE_NO_PAD.encode(t.name.as_bytes()),
                    "value": URL_SAFE_NO_PAD.encode(t.value.as_bytes()),
                })
            })
            .collect();
        serde_json::json!({
            "format": 2,
            "id": self.id_b64(),
            "last_tx": URL_SAFE_NO_PAD.encode(&self.last_tx),
            "owner": URL_SAFE_NO_PAD.encode(&self.owner),
            "tags": tags,
            "target": "",
            "quantity": "0",
            "data": URL_SAFE_NO_PAD.encode(&self.data),
            "data_size": self.data.len().to_string(),
            "data_root": URL_SAFE_NO_PAD.encode(self.data_root),
            "reward": self.reward,
            "signature": URL_SAFE_NO_PAD.encode(&self.signature),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pss::Signature;
    use rsa::pss::VerifyingKey;
    use rsa::signature::Verifier;
    use rsa::RsaPublicKey;

    /// Vecteur deep hash indépendant. Entrée `[b"1", b"abc", [[b"name",
    /// b"value"]]]` (couvre blob, liste, et liste imbriquée type tags). Sortie
    /// épinglée depuis une implémentation de référence SÉPARÉE (transcrite de
    /// `arweave-js` deepHash.ts, exécutée en Python avec hashlib.sha384) —
    /// jamais une auto-assertion de notre code contre lui-même.
    #[test]
    fn deep_hash_matches_independent_reference_vector() {
        let item = DeepHashItem::List(vec![
            DeepHashItem::Blob(b"1"),
            DeepHashItem::Blob(b"abc"),
            DeepHashItem::List(vec![DeepHashItem::List(vec![
                DeepHashItem::Blob(b"name"),
                DeepHashItem::Blob(b"value"),
            ])]),
        ]);
        let got = deep_hash(&item);
        let expected = "f088669b81ba6d8e93e09dc589844c66058ca3680493748b93d356fb845cdc0d87c0aadecf5bc6ff1f0693292d637f73";
        assert_eq!(hex(&got), expected);
    }

    /// Second vecteur deep hash indépendant : un blob simple `b"champinium"`.
    #[test]
    fn deep_hash_single_blob_matches_reference() {
        let got = deep_hash(&DeepHashItem::Blob(b"champinium"));
        let expected = "986a0de9339fe36356ec9fc05447f539bdb947a5e30d76f1e7bf6293d93224657ca3b50b304743e603f3e24e07dc0f8a";
        assert_eq!(hex(&got), expected);
    }

    /// Vecteur data_root (chunk unique, données < 256 Kio) épinglé depuis la
    /// même implémentation de référence indépendante (merkle.ts / Python).
    #[test]
    fn data_root_single_chunk_matches_reference() {
        let data = b"champinium archive test payload";
        let got = data_root(data);
        let expected = "44f1e11104322d4d85de90929d5ab6f12b6e87da4bbfd62da2d59eca73b24300";
        assert_eq!(hex(&got), expected);
    }

    /// Vecteur data_root multi-chunks (300 Kio → deux chunks, pas de
    /// rééquilibrage : reliquat 44 Kio ≥ 32 Kio). Génère les octets
    /// déterministes `(i*7+3) mod 256`, comme la référence.
    #[test]
    fn data_root_multi_chunk_matches_reference() {
        let data: Vec<u8> = (0..300 * 1024).map(|i| ((i * 7 + 3) % 256) as u8).collect();
        let got = data_root(&data);
        let expected = "cab2d1227598940e3793b8fceaec687af334503df60873951718d2d08dc84c4d";
        assert_eq!(hex(&got), expected);
    }

    /// Vecteur data_root déclenchant le rééquilibrage du dernier chunk : 272
    /// Kio (256 + 16 Kio) — le reliquat de 16 Kio < 32 Kio force le partage en
    /// deux moitiés. Octets `(i*13+5) mod 256`.
    #[test]
    fn data_root_rebalance_matches_reference() {
        let data: Vec<u8> = (0..(256 * 1024 + 16 * 1024))
            .map(|i| ((i * 13 + 5) % 256) as u8)
            .collect();
        let got = data_root(&data);
        let expected = "a9b662c99f9bed20158378aeb584698f840e2b75ee214462bbe52e9f138e3268";
        assert_eq!(hex(&got), expected);
    }

    /// Aller-retour signature : signer les octets à signer puis les **vérifier
    /// avec la clé PUBLIQUE** du même JWK (mêmes paramètres PSS : SHA-256, sel
    /// 32). Prouve la cohérence sign/verify de notre chaîne — indépendamment
    /// d'un vrai nœud Arweave. Clé RSA 2048 générée à la volée (la taille
    /// n'affecte pas la correction de la cohérence sign/verify).
    #[test]
    fn signature_round_trips_with_public_key() {
        let mut rng = getrandom::rand_core::UnwrapErr(getrandom::SysRng);
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("génération clé RSA de test");
        let owner = big_endian_of_modulus(&private);
        let wallet = WalletKey {
            private: private.clone(),
            owner,
        };

        let message = deep_hash(&DeepHashItem::Blob(b"octets a signer format 2"));
        let sig_bytes = wallet.sign(&message).expect("signature");

        let verifying_key = VerifyingKey::<Sha256>::new(RsaPublicKey::from(&private));
        let signature = Signature::try_from(sig_bytes.as_slice()).expect("signature bien formee");
        verifying_key
            .verify(&message, &signature)
            .expect("la signature doit se verifier avec la cle publique du meme JWK");
    }

    /// Aller-retour de bout en bout du montage de transaction : construire une
    /// transaction signée via l'API publique du module, puis re-vérifier sa
    /// signature contre les octets à signer recalculés (deep hash des mêmes
    /// champs) avec la clé publique. Prouve que `build_signed` signe bien les
    /// octets que le format impose.
    #[test]
    fn built_transaction_signature_verifies_over_its_signature_data() {
        let mut rng = getrandom::rand_core::UnwrapErr(getrandom::SysRng);
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("génération clé RSA de test");
        let owner = big_endian_of_modulus(&private);
        let wallet = WalletKey {
            private: private.clone(),
            owner: owner.clone(),
        };

        let data = b"contenu de publication a archiver".to_vec();
        let tags = vec![
            Tag {
                name: "champinium-cid".into(),
                value: "bafytest".into(),
            },
            Tag {
                name: "champinium-schema".into(),
                value: "hls/v1".into(),
            },
        ];
        let last_tx = vec![7u8; 32];
        let reward = "12345".to_string();
        let tx =
            Transaction::build_signed(&wallet, data.clone(), tags, last_tx.clone(), reward.clone())
                .expect("construction transaction");

        // Recompose les octets à signer indépendamment et vérifie.
        let dr = data_root(&data);
        let sig_data = deep_hash(&DeepHashItem::List(vec![
            DeepHashItem::Blob(b"2"),
            DeepHashItem::Blob(&owner),
            DeepHashItem::Blob(b""),
            DeepHashItem::Blob(b"0"),
            DeepHashItem::Blob(reward.as_bytes()),
            DeepHashItem::Blob(&last_tx),
            DeepHashItem::List(vec![
                DeepHashItem::List(vec![
                    DeepHashItem::Blob(b"champinium-cid"),
                    DeepHashItem::Blob(b"bafytest"),
                ]),
                DeepHashItem::List(vec![
                    DeepHashItem::Blob(b"champinium-schema"),
                    DeepHashItem::Blob(b"hls/v1"),
                ]),
            ]),
            DeepHashItem::Blob(data.len().to_string().as_bytes()),
            DeepHashItem::Blob(&dr),
        ]));
        let verifying_key = VerifyingKey::<Sha256>::new(RsaPublicKey::from(&private));
        let signature =
            Signature::try_from(tx.signature.as_slice()).expect("signature bien formee");
        verifying_key
            .verify(&sig_data, &signature)
            .expect("la signature de build_signed doit couvrir exactement les champs format 2");

        // L'id est bien SHA-256(signature).
        assert_eq!(tx.id, sha256(&tx.signature).to_vec());
        assert!(!tx.id_b64().is_empty());
    }

    /// Aller-retour du parsing JWK : génère une clé RSA, sérialise ses
    /// composants en JWK (base64url `n`,`e`,`d`,`p`,`q`), la reconstruit via
    /// `WalletKey::from_jwk`, puis signe et **vérifie avec la clé publique
    /// d'origine**. Prouve que le chemin JWK → clé privée (le seul non couvert
    /// par ailleurs sans vrai portefeuille financé) est correct.
    #[test]
    fn wallet_key_from_jwk_round_trips_signature() {
        use rsa::traits::{PrivateKeyParts, PublicKeyParts};

        let mut rng = getrandom::rand_core::UnwrapErr(getrandom::SysRng);
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("génération clé RSA de test");
        let primes = private.primes();
        let b64 = |u: &BoxedUint| URL_SAFE_NO_PAD.encode(u.to_be_bytes_trimmed_vartime());
        let jwk = serde_json::json!({
            "kty": "RSA",
            "n": URL_SAFE_NO_PAD.encode(private.n().to_be_bytes_trimmed_vartime()),
            "e": b64(private.e()),
            "d": b64(private.d()),
            "p": b64(&primes[0]),
            "q": b64(&primes[1]),
        });

        let wallet = WalletKey::from_jwk(&jwk).expect("JWK bien formé doit se parser");
        // owner = module `n` brut (base64url-décodé), tel qu'utilisé dans le
        // deep hash et pour l'adresse du portefeuille.
        assert_eq!(wallet.owner(), &*private.n().to_be_bytes_trimmed_vartime());

        let message = deep_hash(&DeepHashItem::Blob(b"jwk round-trip"));
        let sig_bytes = wallet.sign(&message).expect("signature via clé du JWK");
        let verifying_key = VerifyingKey::<Sha256>::new(RsaPublicKey::from(&private));
        let signature = Signature::try_from(sig_bytes.as_slice()).expect("signature bien formee");
        verifying_key
            .verify(&message, &signature)
            .expect("la clé reconstruite depuis le JWK doit signer de façon vérifiable");
    }

    /// Un JWK amputé d'un champ requis (`p`) est rejeté proprement, sans panique.
    #[test]
    fn wallet_key_from_jwk_rejects_missing_field() {
        let jwk = serde_json::json!({ "kty": "RSA", "n": "AQAB", "e": "AQAB", "d": "AQAB" });
        assert!(WalletKey::from_jwk(&jwk).is_err());
    }

    /// Encodage hexadécimal minuscule pour comparer aux vecteurs de référence.
    fn hex(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    /// Octets big-endian du module `n` d'une clé RSA (pour reconstruire un
    /// `owner` de test à partir d'une clé générée).
    fn big_endian_of_modulus(key: &RsaPrivateKey) -> Vec<u8> {
        use rsa::traits::PublicKeyParts;
        key.n().to_be_bytes().to_vec()
    }
}
