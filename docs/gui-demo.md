# Démo GUI à deux machines — publication → découverte → lecture à l'écran

Rejoue le critère MVP (`docs/mvp-demo.md`, déjà validé en CLI/localhost) avec
**deux machines physiques** et la **lecture dans l'app native**. C'est la
validation visuelle qui reste : catalogue réactif, clic « Lire », vidéo à
l'écran.

Rôles :

- **Machine A — créateur** : publie via `champinium-cli` (les fronts GUI n'ont
  pas d'UI d'ingestion — c'est un outil de créateur, volontairement CLI à ce
  stade). N'importe quel OS.
- **Machine B — spectateur** : l'app GUI (macOS, Windows ou Linux).

Prérequis réseau : les deux machines sur le **même LAN** (pas de NAT entre
elles — la traversée NAT par relais est testée par ailleurs), et le port TCP
choisi (4711 ci-dessous) autorisé en entrée sur A.

## 0. Récupérer les artefacts (release v0.6.0+)

Sur https://github.com/dgrauet/champinium/releases :

- Machine A : `champinium-tools-<os>` (contient `champinium-cli`).
- Machine B : `Champinium-macos.zip` **ou** `Champinium-windows-x86_64.zip`
  **ou** `Champinium-linux-x86_64.tar.gz`.

Ouverture sans signature payante (détails dans `docs/packaging.md`) :

- **macOS** — app : clic droit → Ouvrir → Ouvrir. Outils CLI téléchargés :
  retirer la quarantaine avant le premier lancement :
  `xattr -d com.apple.quarantine champinium-cli` (idem pour les autres binaires).
- **Windows** — SmartScreen : « Informations complémentaires » → « Exécuter
  quand même ».
- **Linux** — installer les dépendances du README embarqué (GTK4 + plugins
  GStreamer, dont `gstreamer1.0-libav` pour h264).

Machine A a besoin de **ffmpeg** (`brew install ffmpeg` / `winget install
ffmpeg` / `apt install ffmpeg`).

## 1. Machine A — publier une vidéo

Une vraie vidéo à soi, ou une vidéo de test :

```sh
ffmpeg -f lavfi -i "testsrc=duration=30:size=1280x720:rate=25" \
       -f lavfi -i "sine=frequency=440:duration=30" \
       -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest demo.mp4

./champinium-cli --data-dir ./machine-a \
    ingest demo.mp4 --listen /ip4/0.0.0.0/tcp/4711 \
    --title "Démo Champinium" --tag demo --tag nature
```

Noter les deux lignes de sortie :

```
Manifeste HLS: bafkrei…
Adresse: /ip4/0.0.0.0/tcp/4711/p2p/12D3KooW…
```

Construire l'adresse à donner à B avec l'**IP LAN réelle** de A (`ipconfig
getifaddr en0` sur macOS, `ip a` sur Linux, `ipconfig` sur Windows) :

```
/ip4/<IP-LAN-de-A>/tcp/4711/p2p/<PeerId-de-A>
```

Laisser la commande tourner (elle sert le contenu et rediffuse le feed).

## 2. Machine B — l'app GUI

1. Lancer l'app. Vérifier l'en-tête : « nœud en ligne » + PeerId affiché.
2. Coller l'adresse de A dans le champ `/ip4/…/tcp/…/p2p/<peerid>` →
   **Connecter**.
3. ✅ **Découverte réactive** : le catalogue apparaît de lui-même en ~1–3 s
   (aucun bouton à presser — c'est le flux d'événements du contrat v3).
   L'entrée montre « Démo Champinium » avec ses tags `demo · nature`.
4. ✅ **Recherche** : taper `nature` (ou `démo`) dans le champ de recherche —
   l'entrée est filtrée par titre/tag. Effacer pour revenir au catalogue.
5. ✅ **Lecture** : cliquer **Lire**. Statut « récupération… » puis « lecture
   en cours » : la vidéo joue dans le lecteur natif (AVPlayer /
   MediaPlayerElement / GStreamer).

## 3. B est devenu seeder (persistance)

Pendant que B garde son app ouverte, **couper A** (Ctrl-C). Puis, au choix :

- **Depuis A** (ou une 3ᵉ machine), récupérer le contenu **depuis B** — il faut
  le PeerId de B (affiché dans l'en-tête de l'app) et son IP LAN :

  ```sh
  ./champinium-cli --data-dir ./verif \
      fetch-hls <Manifeste-HLS> \
      --peer /ip4/<IP-LAN-de-B>/tcp/<port-de-B>/p2p/<PeerId-de-B> \
      --out ./depuis-b
  ```

  ⚠️ L'app GUI écoute sur un **port aléatoire**. Pour un port fixe côté B,
  utiliser à la place le démon : `champinium-seed --data-dir <data-dir-app>
  --listen /ip4/0.0.0.0/tcp/4712` (le data dir de l'app est indiqué dans
  `docs/packaging.md` ; sur macOS : `~/Library/Application Support/Champinium`).

- ✅ Si `fetch-hls` réussit avec A éteint : **le contenu a survécu à son
  publieur** — seed-what-you-consume constaté sur du vrai matériel.

## Grille de validation

| # | Critère | Constat attendu |
|---|---|---|
| 1 | A publie | CID du manifeste + adresse affichés |
| 2 | B la voit | catalogue apparaît **sans action**, titre + tags corrects |
| 3 | Recherche | filtre par titre et par tag |
| 4 | B la regarde | vidéo + son dans le lecteur natif, ~30 s |
| 5 | B seede | `fetch-hls` depuis B réussit **avec A éteint** |
| 6 | Modération visible *(bonus)* | un CID couvert par une denylist souscrite affiche « contenu bloqué par la modération » (pas une erreur technique) |

## Dépannage

- **Catalogue vide après Connecter** : vérifier l'IP LAN (pas 127.0.0.1 ni
  0.0.0.0), le pare-feu de A (port 4711 entrant), et que la commande `ingest`
  de A tourne toujours. Le statut « connexion : … » en bas de l'app donne
  l'erreur typée (réseau vs entrée invalide).
- **Lecture noire / muette sur Linux** : plugins GStreamer manquants
  (`gstreamer1.0-libav`, `-plugins-good`, `-plugins-bad`).
- **« introuvable » au clic Lire** : A s'est arrêté avant que B ait récupéré —
  relancer l'étape 1 (le `--data-dir` conserve identité et contenu).
- Les deux machines gardent leur identité et leur cache entre lancements
  (répertoire de données durable par OS) : la démo est rejouable sans repartir
  de zéro.

Résultat attendu : les 5 (ou 6) cases cochées → le critère MVP est validé
**en GUI sur matériel réel**, dernière réserve de la Phase 3 levée (à noter
dans `CLAUDE.md` et le spec après la session).
