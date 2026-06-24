# Phase 1 — démo P2P entre deux nœuds

Preuve de la Phase 1 : un contenu publié par un nœud est **découvert via la DHT**
et **téléchargé** par un autre, avec vérification d'intégrité par CID.

## Build

```sh
cargo build --workspace
```

## Démo deux nœuds (loopback)

Terminal A — publie un fichier et reste en ligne pour le servir :

```sh
echo "bonjour Champinium" > /tmp/media.bin
cargo run -p champinium-cli -- --data-dir /tmp/a \
    add /tmp/media.bin --listen /ip4/0.0.0.0/tcp/4801
# Affiche :
#   CID: bafkrei...
#   PeerId: 12D3Koo...
#   Adresse: /ip4/127.0.0.1/tcp/4801/p2p/12D3Koo...
```

Terminal B — récupère le bloc par CID depuis A :

```sh
cargo run -p champinium-cli -- --data-dir /tmp/b \
    get <CID> --peer /ip4/127.0.0.1/tcp/4801/p2p/<PeerId-de-A> --out /tmp/out.bin
cmp /tmp/media.bin /tmp/out.bin && echo "transfert P2P OK"
```

`get` vérifie le CID du bloc reçu et le **remet en cache** (seed-what-you-consume) :
B devient à son tour fournisseur.

## Lancer son propre nœud bootstrap (stateless)

```sh
cargo run -p champinium-bootstrap -- --listen /ip4/0.0.0.0/tcp/4101
# Affiche son multiaddr ; référez-le via --bootstrap (commande `serve`).
```

Le bootstrap ne stocke aucun contenu ; il ne persiste que sa clé d'identité pour
offrir un PeerId/multiaddr stable. N'importe qui peut en lancer un.

## Vérification automatisée

- `cargo test -p champinium-core` — tests unitaires (CID, blockstore, identité) +
  test d'intégration `two_nodes_provide_discover_and_transfer` (deux nœuds en
  mémoire : provide → découverte DHT → transfert → vérification).

## Limites connues (Phase 1)

- Transfert via **request-response** (interim) ; **bitswap** viendra plus tard.
- Pas encore de feeds signés / catalogue CRDT / ingestion ffmpeg (Phase 2).
- Pas de modération active sur le chemin (checkpoints Phase 2/3).
- Découverte DHT à 2 nœuds : on ajoute l'adresse du pair explicitement ; le
  routage multi-sauts s'éprouvera avec un bootstrap et plus de nœuds.
