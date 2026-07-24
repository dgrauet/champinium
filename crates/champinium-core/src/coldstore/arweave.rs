//! Backend Arweave du trait `ColdStore` — **récupération seule**.
//!
//! `retrieve` : découverte par tag GraphQL (`champinium-cid: <cid>`) sur
//! chaque gateway configurée, essayées **en séquence** avec un timeout court ;
//! sur trouvaille, téléchargement puis **vérification stricte du CID** — une
//! gateway n'est jamais de confiance, elle peut au pire servir du silence,
//! jamais du faux (voir la spec). L'archivage (signature + upload) est
//! **différé** (voir [`super`]) : aucune dépendance `rsa` n'est tirée. HTTP
//! pur (`reqwest`).

use crate::coldstore::ColdStore;
use crate::content;
use crate::error::Result as CoreResult;
use cid::Cid;
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

/// Timeout par gateway — court : une gateway lente ou muette ne doit pas
/// bloquer le repli d'archive, on passe à la suivante.
const GATEWAY_TIMEOUT_SECS: u64 = 8;

/// Borne la taille d'une réponse de gateway (cohérent avec `MAX_BLOCK_SIZE`
/// dans `p2p.rs`) : une gateway — même malveillante, hors de tout contrôle,
/// c'est tout le modèle de menace de ce module — ne doit jamais pouvoir
/// gonfler la mémoire du client par une réponse énorme. Dépassement → traité
/// comme un échec de cette gateway (gateway suivante), jamais une erreur.
const MAX_ARCHIVE_FETCH_BYTES: u64 = 64 * 1024 * 1024;

/// Backend Arweave : une liste de gateways essayées en séquence.
pub struct ArweaveColdStore {
    gateways: Vec<String>,
    http: Client,
}

impl ArweaveColdStore {
    /// Construit un backend contre la liste de gateways donnée (ex.
    /// `https://arweave.net`), essayées dans l'ordre fourni.
    pub fn new(gateways: Vec<String>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(GATEWAY_TIMEOUT_SECS))
            .build()
            .expect("client HTTP reqwest (rustls) toujours constructible avec cette config");
        Self { gateways, http }
    }

    /// Interroge le GraphQL d'une gateway pour le tag `champinium-cid`,
    /// renvoie l'identifiant de la première transaction trouvée (`None` sur
    /// tout échec réseau/parsing/absence de résultat — jamais une erreur, on
    /// passe simplement à la gateway suivante).
    async fn query_tx_id(&self, gateway: &str, cid_str: &str) -> Option<String> {
        let url = format!("{}/graphql", gateway.trim_end_matches('/'));
        let query = format!(
            "query {{ transactions(tags: [{{ name: \"champinium-cid\", values: [\"{cid_str}\"] }}], first: 1) {{ edges {{ node {{ id }} }} }} }}"
        );
        let body = serde_json::json!({ "query": query });
        let resp = self.http.post(&url).json(&body).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let parsed: GraphQlResponse = resp.json().await.ok()?;
        parsed
            .data
            .transactions
            .edges
            .into_iter()
            .next()
            .map(|edge| edge.node.id)
    }
}

#[derive(Debug, Deserialize)]
struct GraphQlResponse {
    data: GraphQlData,
}

#[derive(Debug, Deserialize)]
struct GraphQlData {
    transactions: GraphQlTransactions,
}

#[derive(Debug, Deserialize)]
struct GraphQlTransactions {
    edges: Vec<GraphQlEdge>,
}

#[derive(Debug, Deserialize)]
struct GraphQlEdge {
    node: GraphQlNode,
}

#[derive(Debug, Deserialize)]
struct GraphQlNode {
    id: String,
}

/// Lit le corps d'une réponse en le bornant à `limit` octets : rejette tôt si
/// `Content-Length` l'annonce déjà au-delà, sinon accumule par morceaux
/// (`Response::chunk`, disponible sans feature `stream`) et abandonne dès que
/// le total dépasse la borne — jamais un `bytes().await` non borné sur une
/// source non fiable. `None` = dépassement ou erreur réseau, traité comme un
/// simple échec de gateway par l'appelant.
async fn read_bounded(mut resp: reqwest::Response, limit: u64) -> Option<Vec<u8>> {
    if resp.content_length().is_some_and(|len| len > limit) {
        return None;
    }
    let mut buf = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                buf.extend_from_slice(&chunk);
                if buf.len() as u64 > limit {
                    return None;
                }
            }
            Ok(None) => return Some(buf),
            Err(_) => return None,
        }
    }
}

#[async_trait::async_trait]
impl ColdStore for ArweaveColdStore {
    async fn retrieve(&self, cid: Cid) -> CoreResult<Option<Vec<u8>>> {
        let cid_str = cid.to_string();
        for gateway in &self.gateways {
            let Some(tx_id) = self.query_tx_id(gateway, &cid_str).await else {
                continue;
            };
            let url = format!("{}/{tx_id}", gateway.trim_end_matches('/'));
            let Ok(resp) = self.http.get(&url).send().await else {
                continue;
            };
            if !resp.status().is_success() {
                continue;
            }
            let Some(bytes) = read_bounded(resp, MAX_ARCHIVE_FETCH_BYTES).await else {
                continue;
            };
            if content::verify(&cid, &bytes) {
                return Ok(Some(bytes));
            }
            tracing::warn!(
                gateway = %gateway,
                tx_id = %tx_id,
                cid = %cid_str,
                "coldstore: octets renvoyés par la gateway ne correspondent pas au CID demandé — rejetés, gateway suivante"
            );
        }
        Ok(None)
    }
}
