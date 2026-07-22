//! champinium-seed — démon de seeding en arrière-plan (hors UI).
//!
//! Depuis le retrait de seed-what-you-consume (spec channels lot c), le démon
//! ne fait plus que **resservir ce qu'il détient déjà** : au démarrage et
//! périodiquement, il **réannonce** tous ses CIDs dans la DHT (provider
//! records). Il ne publie PLUS de feed — la publication appartient au nœud
//! créateur, pas au démon de seeding. Conçu pour tourner sous launchd (macOS),
//! un service Windows, ou un systemd user service (Linux) — voir
//! `infra/services/`.
//!
//! La modération par défaut reste active : un seeder ne ressert jamais un contenu
//! matché (les checkpoints du noyau s'appliquent au service comme au reste).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use champinium_core::identity::load_or_generate;
use champinium_core::p2p::split_peer_id;
use champinium_core::{Blockstore, Node};
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "champinium-seed",
    about = "Démon de seeding Champinium (hors UI)"
)]
struct Cli {
    /// Répertoire de données du nœud (clé d'identité + blocs à resservir).
    #[arg(long, default_value = ".champinium")]
    data_dir: PathBuf,
    /// Adresse d'écoute.
    #[arg(long, default_value = "/ip4/0.0.0.0/tcp/0")]
    listen: String,
    /// Pairs de bootstrap `/ip4/.../tcp/.../p2p/<peerid>` (répétable).
    #[arg(long)]
    bootstrap: Vec<String>,
    /// Intervalle de réannonce, en secondes.
    #[arg(long, default_value = "3600")]
    reprovide_interval: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "champinium_seed=info,champinium_core=warn".into()),
        )
        .init();

    let cli = Cli::parse();
    let keypair = load_or_generate(cli.data_dir.join("node.key"))?;
    let blockstore = Blockstore::open(cli.data_dir.join("blocks"))?;
    let node = Node::new(keypair, blockstore).await?;

    let addr = node
        .listen(cli.listen.parse().context("multiaddr d'écoute invalide")?)
        .await?;
    tracing::info!(peer = %node.peer_id(), %addr, "seeder en ligne");

    for b in &cli.bootstrap {
        let (pid, base) = split_peer_id(b.parse().context("multiaddr de bootstrap invalide")?)?;
        node.add_address(pid, base.clone()).await?;
        if let Err(e) = node.dial(base).await {
            tracing::warn!("bootstrap {b} injoignable: {e}");
        }
    }

    let interval = Duration::from_secs(cli.reprovide_interval.max(1));
    loop {
        reseed(&node).await;
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("arrêt du seeder");
                return Ok(());
            }
        }
    }
}

/// Réannonce tous les CIDs détenus (provider records). La publication du feed
/// n'appartient PAS au démon — c'est le nœud créateur qui publie ce qu'il
/// crée ; le démon de seeding ne fait que resservir ce qu'il détient déjà.
async fn reseed(node: &Node) {
    match node.reprovide_all().await {
        Ok(n) => tracing::info!("réannonce de {n} CID(s)"),
        Err(e) => tracing::warn!("réannonce échouée: {e}"),
    }
}
