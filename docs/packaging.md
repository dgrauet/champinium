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

- **Runtime** : `org.gnome.Platform`/`org.gnome.Sdk` 47 (stable courante) +
  extension SDK `org.freedesktop.Sdk.Extension.rust-stable` pour builder
  cargo. GStreamer core/plugins-base/plugins-good sont déjà dans le runtime
  GNOME. **Limite connue** : plugins-bad et libav (H.264/AAC — voir la liste
  de dépendances du tarball ci-dessus) n'y sont **pas** inclus ; ce manifeste
  ne les compile pas depuis les sources pour l'instant (suivi documenté,
  pas un blocage du build).
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
- **Icône d'app** : une icône **placeholder** (`org.champinium.Champinium.svg`,
  champignon stylisé) est installée pour satisfaire `appstreamcli`
  (`gui-app-without-icon`) — à remplacer par une vraie identité visuelle.
- **Lecture H.264/AAC** : le runtime `org.gnome.Platform` de base n'embarque pas
  gstreamer-plugins-bad/libav → l'app se lance mais ne lit pas la plupart des
  vidéos. **Bloqueur avant tout usage réel** : ajouter ces plugins en modules
  flatpak-builder.

### Build/installation locale

```sh
flatpak-builder --user --install --force-clean build-dir \
  packaging/flatpak/org.champinium.Champinium.yml
flatpak run org.champinium.Champinium
```

Prérequis : `flatpak`, `flatpak-builder`, et les runtimes
`org.gnome.Platform//47` + `org.gnome.Sdk//47` +
`org.freedesktop.Sdk.Extension.rust-stable//47` installés (`flatpak install
flathub org.gnome.Platform//47 org.gnome.Sdk//47`).

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
