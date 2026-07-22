//! Profil de channel : persisté sous data_dir, porté par les feeds publiés,
//! republié au changement (spec channels §1/§5, lot a).

use champinium_core::feed::ChannelMeta;
use champinium_core::Node;

fn profile(name: &str) -> ChannelMeta {
    ChannelMeta {
        name: name.into(),
        description: "desc".into(),
        avatar_cid: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_survives_restart_and_signs_published_feeds() {
    let dir = tempfile::tempdir().unwrap();

    {
        let node = Node::open(dir.path()).await.unwrap();
        node.set_channel_profile(profile("Aurores")).await.unwrap();
        let cid = champinium_core::content::cid_for(b"contenu");
        node.publish_feed(&[cid]).await.unwrap();
        // Le feed publié (visible dans le catalogue local) porte le profil.
        let entries = node.catalog_entries();
        assert_eq!(entries[0].channel.name, "Aurores");
    }

    // Redémarrage : le profil est rechargé depuis .channel_profile.
    let node = Node::open(dir.path()).await.unwrap();
    assert_eq!(node.channel_profile().name, "Aurores");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changing_profile_republishes_current_feed() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(dir.path()).await.unwrap();
    let cid = champinium_core::content::cid_for(b"contenu");
    node.publish_feed(&[cid]).await.unwrap();
    let seq_before = node.catalog_entries()[0].seq;

    node.set_channel_profile(profile("Nouveau nom"))
        .await
        .unwrap();

    let entries = node.catalog_entries();
    assert_eq!(entries[0].channel.name, "Nouveau nom");
    assert_eq!(entries[0].cids, vec![cid], "les entries sont conservées");
    assert!(
        entries[0].seq > seq_before,
        "republication = nouveau seq (LWW)"
    );
}
