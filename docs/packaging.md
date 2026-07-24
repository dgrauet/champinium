# Packaging — palier GRATUIT (sans compte de signature)

Phase 6 en deux paliers. Celui-ci ne coûte **rien** : pas de compte Apple
Developer (99 $/an), pas de certificat Authenticode. En contrepartie, les OS
affichent leurs avertissements « éditeur non vérifié » — instructions
d'ouverture ci-dessous, à communiquer aux testeurs.

Les artefacts sont construits par le workflow
[`release-artifacts.yml`](../.github/workflows/release-artifacts.yml) et
attachés à chaque release GitHub (déclenchement : publication d'une release
par release-please ; test à blanc possible via *Run workflow*).

## Artefacts

| Fichier | Contenu |
|---|---|
| `Champinium-macos.zip` | `Champinium.app` (arm64), signée **ad-hoc**, non notarisée |
| `Champinium-windows-x86_64.zip` | dossier portable non signé (WinAppSDK auto-contenu, aucun runtime à installer) |
| `Champinium-linux-x86_64.tar.gz` | binaire GTK4 + `.desktop` + README (dépend des libs système) |
| `champinium-tools-{macos-arm64,linux-x86_64,windows-x86_64}.*` | `champinium-cli`, `champinium-seed`, `champinium-bootstrap`, `champinium-relay` |

## Ouvrir l'app malgré l'absence de signature payante

- **macOS** : premier lancement → clic droit sur `Champinium.app` → **Ouvrir**
  → Ouvrir. (Alternative : `xattr -d com.apple.quarantine Champinium.app`.)
  La signature ad-hoc garantit l'intégrité locale du bundle, pas l'identité de
  l'éditeur.
- **Windows** : SmartScreen affiche « Windows a protégé votre ordinateur » →
  **Informations complémentaires** → **Exécuter quand même**. Dézipper puis
  lancer `Champinium.exe` (tout est auto-contenu, `champinium_core.dll`
  comprise).
- **Linux** : pas de barrière de signature ; installer les dépendances système
  (GTK4 + plugins GStreamer, voir le README du tarball) puis `./champinium`.

## Build local

- macOS : `just macos-app` → `dist/Champinium-macos.zip`
  (assemble le bundle, rebase la lib native sur `@rpath`, signe ad-hoc).
- Linux : `cargo build --release -p champinium-linux --features gui` puis
  `./scripts/package-linux-app.sh`.
- Windows : `just gen-csharp` puis
  `dotnet publish apps/windows/Champinium/Champinium.csproj -c Release -r win-x64 -p:Platform=x64 -p:WindowsAppSDKSelfContained=true -p:SelfContained=true`.

## Linux — Flatpak

Palier Linux au-delà du tarball : un manifeste Flatpak pour le front GTK4,
[`packaging/flatpak/org.champinium.Champinium.yml`](../packaging/flatpak/org.champinium.Champinium.yml)
(app-id `org.champinium.Champinium`), plus le `.desktop` et le métainfo
AppStream requis à côté. Toujours palier **gratuit** (0 €, pas de compte
Flathub) — ce n'est pas une soumission Flathub, juste un paquet installable
localement ou distribuable en `.flatpak` autonome.

- **Runtime** : `org.gnome.Platform`/`org.gnome.Sdk` 48 (base freedesktop
  24.08). rustc est installé au build via rustup (l'extension SDK `rust-stable`
  est trop ancienne pour la pile gtk-rs courante — voir supply-chain ci-dessous).
  GStreamer core/plugins-base/plugins-good sont déjà dans le runtime GNOME.
- **Lecture H.264/AAC** : fournie par l'extension
  `org.freedesktop.Platform.ffmpeg-full//24.08` (`add-extensions`, montée dans
  `/app/lib/ffmpeg` avec `add-ld-path`). Le runtime embarque gst-libav mais lié
  à l'ffmpeg de base freedesktop, amputé des décodeurs sous brevet ; l'extension
  monte un ffmpeg complet en tête du LD_LIBRARY_PATH, et gst-libav y trouve
  H.264/AAC. Patron Flathub standard (codecs encombrés distribués à part, jamais
  dans l'app). Le front étant lecture seule (playbin), seul le décodage est
  requis. Champinium ingérant en H.264/AAC (HLS), l'extension est nécessaire
  pour lire le propre contenu de l'app.
- **Permissions (`finish-args`)** : réseau (libp2p), wayland/fallback-x11/dri
  (fenêtre GTK4 + rendu vidéo GStreamer), ipc, pulseaudio (audio). **Pas de**
  `--filesystem=host` ni `--filesystem=xdg-download` : les données du nœud
  (`champinium-core::paths::default_data_dir()` → `$XDG_DATA_HOME/champinium`)
  atterrissent automatiquement, sous Flatpak, dans
  `~/.var/app/org.champinium.Champinium/data` par la redirection standard de
  `XDG_DATA_HOME` par le sandbox — aucune permission supplémentaire requise
  pour que l'identité/les blocs persistent entre lancements.
- **Chaîne d'approvisionnement du build (durcissement requis avant Flathub)** :
  deux vecteurs réseau non reproductibles au build, à supprimer ensemble pour
  une publication réelle —
    - **Sources cargo** : build avec `--share=network` (cargo télécharge les
      crates), PAS de vendoring hors-ligne (`cargo-sources.json` via
      `flatpak-cargo-generator`). Flathub exige des sources vendorisées pour la
      reproductibilité des modules cargo.
    - **Toolchain rustc** : installé au build via `curl https://sh.rustup.rs | sh`
      (l'extension SDK `rust-stable` de GNOME est trop ancienne pour la pile
      gtk-rs courante, qui exige rustc ≥ 1.92). Ce `curl | sh` exécute un
      script distant **non épinglé** — vecteur supply-chain relevé en revue.
      Un vrai build doit épingler rustup (URL + somme de contrôle de
      `rustup-init`, toolchain figé) ou fournir rustc via une extension SDK à
      jour. Les sources cargo vendorisées ci-dessus supprimeront de toute façon
      le besoin de réseau au build.
- **Icône d'app** : le SVG (`org.champinium.Champinium.svg`, champignon stylisé,
  encore une identité **placeholder**) est la source de vérité, **rasterisé au
  build** (`rsvg-convert`, dans le SDK GNOME) en PNG 128/256 installés dans
  hicolor + le SVG scalable conservé. Une icône raster trouvable est **requise**
  par `appstreamcli compose` : un SVG scalable seul échoue en
  `file-read-error`/`filters-but-no-output` (cause du premier rouge CI). Le
  design reste à remplacer par une vraie identité visuelle, mais la chaîne
  d'icônes est complète et valide.

### Build/installation locale

```sh
flatpak-builder --user --install --force-clean build-dir \
  packaging/flatpak/org.champinium.Champinium.yml
flatpak run org.champinium.Champinium
```

Prérequis : `flatpak`, `flatpak-builder`, et les runtimes
`org.gnome.Platform//48` + `org.gnome.Sdk//48` +
`org.freedesktop.Platform.ffmpeg-full//24.08` installés (`flatpak install
flathub org.gnome.Platform//48 org.gnome.Sdk//48
org.freedesktop.Platform.ffmpeg-full//24.08`). L'extension ffmpeg-full est
tirée automatiquement à l'installation de l'app (`no-autodownload: false`).

### CI

Job `flatpak` dans [`ci.yml`](../.github/workflows/ci.yml) : construit le
manifeste dans le conteneur `bilelmoussaoui/flatpak-github-actions:gnome-47`
via l'action `flatpak/flatpak-github-actions/flatpak-builder`, produit
`champinium.flatpak` en artefact de workflow. C'est un build de
**validation** (le manifeste est correct et se construit) — n'attaque jamais
Flathub. Ce job n'a pas pu être exécuté localement pendant l'écriture de ce
manifeste (pas de Flatpak sur macOS) : c'est ce job CI qui fait foi.

### AppImage (suivi, non fait)

Pas de recette AppImage pour l'instant — différé par effort, comme documenté
plus bas. Candidat naturel d'un prochain lot packaging Linux si Flatpak seul
ne couvre pas un besoin (ex. environnements sans `flatpak` installé).

## Ce que le palier PAYANT ajouterait (différé)

| OS | Coût | Gain |
|---|---|---|
| macOS | Apple Developer 99 $/an | Developer ID + **notarisation** : double-clic direct, pas de contournement Gatekeeper ; canal de distribution .dmg propre |
| Windows | Certificat Authenticode (OV ~100–300 €/an, EV plus cher) | plus d'avertissement SmartScreen (réputation immédiate avec EV) ; MSIX signé installable proprement |
| Linux | 0 € | Flatpak (ce paquet) → **Flathub** (soumission, hors périmètre ici) ; AppImage : gratuit — différé par **effort**, pas par coût |

Limites connues du palier gratuit :
- macOS : arm64 uniquement (runner CI) ; pas de binaire universel.
- Auto-update : aucun mécanisme (télécharger la release suivante).
- La version affichée vient de `.release-please-manifest.json` (le
  `Cargo.toml` du workspace n'est pas bumpé par release-please — écart connu).
