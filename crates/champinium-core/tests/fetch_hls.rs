//! `fetch_hls` ne doit pas laisser de sortie partielle (segments `.ts` orphelins
//! sans `index.m3u8`) quand un segment est introuvable ou refusé.

use champinium_core::content::cid_for;
use champinium_core::identity::load_or_generate;
use champinium_core::ingest::HlsSegment;
use champinium_core::{Blockstore, HlsManifest, Moderation, Node};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_hls_leaves_no_partial_output_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let kp = load_or_generate(dir.path().join("node.key")).unwrap();
    let bs = Blockstore::open(dir.path().join("blocks")).unwrap();
    let node = Node::with_moderation(kp, bs, Moderation::empty())
        .await
        .unwrap();

    // Premier segment présent localement (sera écrit), second introuvable :
    // la reconstruction doit échouer APRÈS avoir écrit le premier → c'est le
    // cas qui laissait un `.ts` orphelin sans `index.m3u8`.
    let present_cid = node.add(b"premier segment bien present").await.unwrap();
    let manifest = HlsManifest::new(
        1.0,
        vec![
            HlsSegment {
                cid: present_cid.to_string(),
                duration: 1.0,
            },
            HlsSegment {
                cid: cid_for(b"segment absent").to_string(),
                duration: 1.0,
            },
        ],
    );
    let manifest_cid = node
        .add(manifest.to_json().unwrap().as_bytes())
        .await
        .unwrap();

    let out = dir.path().join("hls-out");
    let res = tokio::time::timeout(Duration::from_secs(30), node.fetch_hls(manifest_cid, &out))
        .await
        .expect("fetch_hls doit se terminer");

    assert!(
        res.is_err(),
        "un segment manquant doit faire échouer fetch_hls"
    );
    assert!(
        !out.join("index.m3u8").exists(),
        "aucun index.m3u8 ne doit être produit sur échec"
    );
    // Aucun segment .ts orphelin ne doit subsister.
    if out.exists() {
        let leftovers: Vec<_> = std::fs::read_dir(&out).unwrap().collect();
        assert!(
            leftovers.is_empty(),
            "aucune sortie partielle ne doit subsister, trouvé: {leftovers:?}"
        );
    }
}
