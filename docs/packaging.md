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

## Ce que le palier PAYANT ajouterait (différé)

| OS | Coût | Gain |
|---|---|---|
| macOS | Apple Developer 99 $/an | Developer ID + **notarisation** : double-clic direct, pas de contournement Gatekeeper ; canal de distribution .dmg propre |
| Windows | Certificat Authenticode (OV ~100–300 €/an, EV plus cher) | plus d'avertissement SmartScreen (réputation immédiate avec EV) ; MSIX signé installable proprement |
| Linux | 0 € | Flatpak (Flathub) / AppImage : gratuits — différés par **effort**, pas par coût ; candidats naturels au prochain palier |

Limites connues du palier gratuit :
- macOS : arm64 uniquement (runner CI) ; pas de binaire universel.
- Auto-update : aucun mécanisme (télécharger la release suivante).
- La version affichée vient de `.release-please-manifest.json` (le
  `Cargo.toml` du workspace n'est pas bumpé par release-please — écart connu).
