# Démo — abonnement, seed proactif, persistance sans le publieur

Critère de sortie de la spec channels (lot c) : **B s'abonne au channel de A,
A s'éteint, un nœud C obtient quand même le contenu — depuis B seul**, parce
que B l'a retenu proactivement en tant qu'abonné (et non parce qu'il l'aurait
simplement regardé — seed-what-you-consume est retiré). Cette démo le prouve
avec quatre nœuds isolés (répertoires de données distincts, liés uniquement
par le réseau) — sur une même machine en `127.0.0.1`, ou sur des machines
séparées en remplaçant les adresses.

> Historique : la démo Phase 3 (2026-07-04, v0.2.0) déroulait un enchaînement
> voisin via seed-what-you-consume (voir `CLAUDE.md`, État actuel, Phase 3).
> Ce document a été réécrit pour le nouveau mécanisme de persistance (channels
> lot c) : le contenu n'est plus retenu par un spectateur de passage, mais par
> un abonné, sous quota, avec éviction et pins explicites — voir
> `docs/architecture.md` §6 bis.

Prérequis : `ffmpeg`/`ffprobe` sur le nœud qui ingère (et pour la
vérification), binaires `champinium-cli` et `champinium-seed`
(`cargo build --release`).

> Nuance importante : `champinium-cli subscribe` **persiste** l'abonnement
> puis lance un fetch immédiat et le suivi actif en tâche de fond dans le
> même processus — mais `champinium-cli` est un outil ponctuel : sitôt la
> commande terminée, le processus (et ses tâches de fond) s'arrête. La
> **rétention proactive** (le seed sous quota) a besoin d'un nœud qui reste en
> ligne — `champinium-seed` (ou une app GUI ouverte). C'est exactement le
> partage de rôles voulu par la spec : `subscribe` change *ce qu'on suit* (un
> état local, persisté), le nœud qui tourne décide *ce qu'il retient*.

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

Sortie : le **CID du manifeste HLS** (checkpoint modération #1 appliqué à
chaque segment), le PeerId et l'**adresse** de A. Le contenu publié par A est
**auto-épinglé** dans son propre `SeedIndex` (un créateur ne s'évince jamais
lui-même sous quota). Le nœud reste en ligne (Ctrl-C pour arrêter) et
rediffuse son feed signé périodiquement.

## 2. Machine B — abonnement (« B s'abonne »)

```sh
champinium-cli --data-dir ./machine-b subscribe <PeerId_A> --peer <ADRESSE_A>
# abonné à champinium://channel/<PeerId_A>
```

L'abonnement est **persisté** (fichier `.subscriptions` du répertoire de
données de B) : c'est ce qui compte pour la suite, indépendamment du fait que
ce processus ponctuel se termine aussitôt après.

## 3. Machine B — en ligne, seed proactif (« B retient »)

```sh
champinium-cli --data-dir ./machine-b quota
# utilisé: 0 octet(s) / quota: 21474836480 octet(s)   ← rien retenu pour l'instant

champinium-seed --data-dir ./machine-b --listen /ip4/0.0.0.0/tcp/4712 --bootstrap <ADRESSE_A>
# INFO seeder en ligne …
# (quelques secondes plus tard, une fois le feed de A récupéré et le seed
#  proactif entré en jeu)
# INFO réannonce de 4 CID(s)          ← manifeste + 3 segments, retenus SANS lecture explicite
```

Au démarrage, `champinium-seed` recharge les abonnements persistés à l'étape
2 (rattrapage), se connecte à A via `--bootstrap`, récupère son feed, puis la
**boucle de seed proactif** du noyau retient le manifeste et ses segments dans
le `SeedIndex` de B (sous quota — 20 Gio par défaut, largement suffisant pour
10 s de test). Laisser tourner ce processus — c'est lui qui sert C plus bas.

Dans un autre terminal, confirmer sans interrompre B :

```sh
champinium-cli --data-dir ./machine-b replication <CID_manifeste> --peer <ADRESSE_B>
# facteur de réplication: 2 fournisseur(s)   ← A (créateur) + B (abonné, seed proactif)
```

## 4. Machine A — hors ligne

```sh
# couper A (Ctrl-C sur le terminal de l'étape 1)
```

B (toujours en ligne comme `champinium-seed`) est inchangé : il sert déjà le
contenu depuis son propre `SeedIndex`, sans dépendre de A.

## 5. Machine C — persistance sans le publieur (« C obtient depuis B seul »)

```sh
champinium-cli --data-dir ./machine-c subscribe <PeerId_A> --peer <ADRESSE_B>
champinium-cli --data-dir ./machine-c fetch-hls <CID_manifeste> --peer <ADRESSE_B> --out ./out-c
ffprobe -v error -show_entries format=format_name,duration ./out-c/index.m3u8
# format_name=hls, duration=10.000000 — jouable (AVPlayer/GStreamer/ffplay)
```

C se connecte à **B**, pas à A (éteint, injoignable), et récupère le
manifeste et les segments : B les sert parce qu'il les a **retenus
proactivement en tant qu'abonné** (étape 3), pas parce qu'il les aurait
simplement regardés. Sans l'abonnement + le seed proactif de l'étape 2-3, B
n'aurait rien à servir.

## 6. Vue Explorer — regarder sans s'abonner ne laisse pas de trace fournisseur

Un nœud **D**, non abonné, peut quand même découvrir A par le catalogue
complet (vue **Explorer**, opt-in derrière un avertissement) et lire son
contenu tant qu'un fournisseur (ici B) est joignable :

```sh
champinium-cli --data-dir ./machine-d fetch-hls <CID_manifeste> --peer <ADRESSE_B> --out ./out-d
champinium-cli --data-dir ./machine-d replication <CID_manifeste> --peer <ADRESSE_B>
# facteur de réplication: 2 fournisseur(s)   ← INCHANGÉ après la lecture de D
```

`out-d` est identique octet pour octet à `out-c`/`out-b` (`shasum`), mais le
PeerId de D **n'apparaît pas** parmi les fournisseurs du CID après coup :
`get` en politique `Stream` (le défaut de toute lecture) ne met pas le bloc en
cache chez D et ne l'annonce pas comme fournisseur. Regarder en vue Explorer
ne persiste plus rien — seul un abonnement retenu par un nœud en ligne (étapes
2-3) le fait.

## 7. Désabonnement — la purge (pins exceptés)

```sh
# arrêter B (Ctrl-C sur le terminal champinium-seed de l'étape 3) avant
# d'agir en CLI sur le même répertoire de données — pas d'accès concurrent.

champinium-cli --data-dir ./machine-b unsubscribe <PeerId_A>
champinium-cli --data-dir ./machine-b quota
# utilisé: 0 octet(s) / quota: 21474836480 octet(s)   ← purgé
```

Se désabonner purge du `SeedIndex` de B les publications **non épinglées** de
A ; les blocs orphelins sont supprimés du blockstore. Pour vérifier la survie
des pins : refaire les étapes 2-3, puis épingler avant de se désabonner
(`champinium-cli --data-dir ./machine-b pin <CID_manifeste>`) — ce manifeste
précis survit à la purge malgré le désabonnement, et `quota` continue de
compter ses octets.

## Ce que la démo prouve

| Critère (spec channels lot c) | Preuve |
|---|---|
| A publie, contenu auto-épinglé | ingestion ffmpeg → CID manifeste ; `SeedIndex` de A épingle sa propre publication |
| B s'abonne → seed proactif | `quota` de B passe de 0 à 4 CID retenus (`réannonce de 4 CID(s)`) sans lecture explicite |
| Persistance sans le publieur | C, abonné, obtient le contenu identique **depuis B seul**, A éteint |
| Lecture Explorer ne persiste rien | D lit avec succès mais n'apparaît jamais comme fournisseur (réplication inchangée) |
| Désabonnement purge (pins exceptés) | `quota` de B retombe à 0 après `unsubscribe`, sauf un manifeste explicitement épinglé |

> Équivalent GUI : les trois fronts font le même enchaînement (openNode →
> connect → catalogue réactif Abonnements/Explorer → lecture native →
> indicateur de seed « à jour » / « seed en cours (x/y) » par channel, actions
> pin/unpin, réglage du quota). La lecture AVPlayer/GStreamer/MediaPlayerElement
> à l'écran reste à valider visuellement hors headless.
