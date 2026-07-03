# Démo MVP — publication → découverte → lecture → reseed

Critère de sortie de la Phase 3 (spec) : **A publie une vidéo, B la voit,
clique, la regarde, devient seeder.** Cette démo le prouve avec trois nœuds
isolés (répertoires de données distincts, liés uniquement par le réseau) — sur
une même machine en `127.0.0.1`, ou sur des machines séparées en remplaçant les
adresses. Déroulée avec succès le 2026-07-04 (v0.2.0).

Prérequis : `ffmpeg`/`ffprobe` sur le nœud qui ingère (et pour la vérification),
binaires `champinium-cli` et `champinium-seed` (`cargo build --release`).

## 0. Une vraie vidéo de test (10 s, h264 + AAC)

```sh
ffmpeg -f lavfi -i "testsrc=duration=10:size=640x360:rate=25" \
       -f lavfi -i "sine=frequency=440:duration=10" \
       -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest video.mp4
```

## 1. Machine A — publication (créateur)

```sh
champinium-cli --data-dir ./machine-a ingest video.mp4 --listen /ip4/0.0.0.0/tcp/4711
```

Sortie : le **CID du manifeste HLS** (checkpoint modération #1 appliqué à chaque
segment), le PeerId et l'**adresse** de A. Le nœud reste en ligne et rediffuse
son feed signé périodiquement.

## 2. Machine B — découverte (« B la voit »)

```sh
champinium-cli --data-dir ./machine-b catalog --peer <ADRESSE_A>
# créateur <PeerId_A> (seq N) :
#   <CID_manifeste>
```

Le catalogue est reconstruit **en écoutant gossipsub** (aucune API centrale).
Hors gossip, `fetch-feed --issuer <PeerId_A>` retrouve le même feed via la DHT.

## 3. Machine B — lecture (« clique, la regarde »)

```sh
champinium-cli --data-dir ./machine-b fetch-hls <CID_manifeste> --peer <ADRESSE_A> --out ./out-b
ffprobe -v error -show_entries format=format_name,duration ./out-b/index.m3u8
# format_name=hls, duration=10.000000 — jouable (AVPlayer/GStreamer/ffplay)
```

Chaque segment est vérifié contre son CID à la réception (checkpoint #2 avant
toute mise en cache), et B **réannonce** ce qu'il consomme (provider records).

## 4. Machine B — seeder (« devient seeder »), preuve : A hors ligne

```sh
# couper A (Ctrl-C), puis relancer B comme démon de seeding :
champinium-seed --data-dir ./machine-b --listen /ip4/0.0.0.0/tcp/4712
# INFO seeder en ligne … réannonce de 4 CID(s)
```

Une machine **C** récupère alors le contenu **depuis B seul** :

```sh
champinium-cli --data-dir ./machine-c fetch-hls <CID_manifeste> --peer <ADRESSE_B> --out ./out-c
```

Vérification observée : `out-c` est **octet pour octet identique** à `out-b`
(`shasum`), alors que le publieur d'origine est hors ligne — le contenu survit
via seed-what-you-consume.

## Ce que la démo prouve

| Critère MVP | Preuve |
|---|---|
| A publie une vidéo | ingestion ffmpeg → 3 segments CID + manifeste, feed signé gossip + DHT |
| B la voit | catalogue reconstruit par écoute gossip (et par GET DHT) |
| B la regarde | `index.m3u8` valide (hls, 10 s, h264 640×360) |
| B devient seeder | C obtient le contenu identique depuis B, **A éteint** |

> Équivalent GUI : les trois fronts font le même enchaînement (openNode →
> connect → catalogue réactif → lecture native). La lecture AVPlayer/GStreamer/
> MediaPlayerElement à l'écran reste à valider visuellement hors headless.
