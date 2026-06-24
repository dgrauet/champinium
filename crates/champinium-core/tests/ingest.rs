//! Test d'intégration Phase 2 : ingestion ffmpeg → HLS → P2P → reconstruction.
//!
//! Le test est ignoré si `ffmpeg` n'est pas disponible (CI sans ffmpeg).

use champinium_core::identity::load_or_generate;
use champinium_core::{Blockstore, Moderation, Node};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

async fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Génère un petit média synthétique (vidéo + audio) de `secs` secondes.
async fn generate_media(out: &Path, secs: u32) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!("testsrc=duration={secs}:size=160x90:rate=15"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={secs}"))
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            "-t",
        ])
        .arg(secs.to_string())
        .arg(out)
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn node(dir: &Path, name: &str) -> Node {
    let kp = load_or_generate(dir.join(format!("{name}.key"))).unwrap();
    let bs = Blockstore::open(dir.join(name)).unwrap();
    Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ingest_segments_and_fetch_hls_over_p2p() {
    if !ffmpeg_available().await {
        eprintln!("ffmpeg absent — test d'ingestion ignoré");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mp4");
    assert!(
        generate_media(&input, 5).await,
        "génération du média de test"
    );

    // A ingère : ffmpeg -> HLS -> segments (CIDs) + manifeste (CID).
    let node_a = node(dir.path(), "a").await;
    let manifest_cid = node_a.ingest_file(&input).await.unwrap();

    // Le manifeste doit lister plusieurs segments (média de 5 s, segments de 4 s).
    let manifest_bytes = node_a.get(manifest_cid).await.unwrap();
    let manifest = champinium_core::HlsManifest::from_json(&manifest_bytes).unwrap();
    assert!(
        manifest.segments.len() >= 2,
        "attendu plusieurs segments, obtenu {}",
        manifest.segments.len()
    );

    // B récupère et reconstruit le HLS via le réseau.
    let node_b = node(dir.path(), "b").await;
    let addr_a = node_a
        .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .await
        .unwrap();
    node_b
        .add_address(node_a.peer_id(), addr_a.clone())
        .await
        .unwrap();
    node_b.dial(addr_a).await.unwrap();

    let out_dir = dir.path().join("b_out");
    let playlist = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(p) = node_b.fetch_hls(manifest_cid, &out_dir).await {
                return p;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    })
    .await
    .expect("reconstruction HLS via P2P dans le délai imparti");

    let pl = tokio::fs::read_to_string(&playlist).await.unwrap();
    assert!(pl.contains("#EXT-X-ENDLIST"));
    // Autant de fichiers .ts que de segments du manifeste.
    let ts_count = std::fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "ts").unwrap_or(false))
        .count();
    assert_eq!(ts_count, manifest.segments.len());
}
