//! champinium-seed — démon de seeding en arrière-plan (hors UI).
//!
//! Depuis le retrait de seed-what-you-consume (spec channels lot c), le démon
//! ne fait que **resservir ce qu'il détient déjà**. La maintenance elle-même —
//! **réannonce** dans la DHT des ROOTS détenus (blockstore moins les segments
//! indexés par une publication : annonce par racine, ADR 0012) et
//! **republication** des feeds SIGNÉS détenus légitimement (le sien s'il a
//! publié, ceux de ses abonnements) — appartient désormais au **nœud**
//! lui-même : `Node::listen` démarre `maintenance_loop`, quel que soit le
//! porteur du nœud (front GUI, CLI ou ce démon). Avant ce déplacement, un
//! utilisateur sans démon installé ne réannonçait jamais rien après un
//! redémarrage, et son contenu s'éteignait au TTL des records Kademlia.
//!
//! Le démon garde donc une seule raison d'être : **servir quand l'application
//! est fermée**. L'app seede tant qu'elle est ouverte ; le démon, le reste du
//! temps. Il ne PUBLIE (crée/incrémente `seq`) toujours PAS de feed — ça reste
//! le rôle du nœud créateur. Conçu pour tourner sous launchd (macOS), un
//! service Windows, ou un systemd user service (Linux) — voir
//! `infra/services/`.
//!
//! La modération par défaut reste active : un seeder ne ressert jamais un contenu
//! matché (les checkpoints du noyau s'appliquent au service comme au reste).

use std::path::PathBuf;

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

    // Découverte initiale (ADR 0013) : rejoint aussi les bootstraps connus du
    // nœud (compilés ∪ persistés `.bootstraps`), best-effort — n'empêche pas
    // le démarrage du démon si aucun n'est joignable.
    match node.bootstrap().await {
        Ok(n) => tracing::info!("bootstrap : {n} dial(s) lancé(s)"),
        Err(e) => tracing::warn!("bootstrap échoué: {e}"),
    }

    // La maintenance périodique tourne déjà dans le nœud (démarrée par le
    // `listen` ci-dessus, première passe immédiate) : le démon n'a plus qu'à
    // rester en vie pour la porter jusqu'à l'arrêt.
    tokio::signal::ctrl_c().await?;
    tracing::info!("arrêt du seeder");
    Ok(())
}
