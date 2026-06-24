# Champinium

Plateforme de **partage P2P de contenu généré par IA** (vidéo, image, audio).
UX esprit Popcorn Time (parcourir → cliquer → ça streame), mais **native sur les
3 OS** (pas d'Electron) et **décentralisée jusque dans la découverte** (pas d'API
centrale).

> ⚠️ Projet à un stade **squelette** : l'arborescence, le workspace et les stubs
> sont posés ; il n'y a **pas encore de logique métier** (P2P, ingestion, lecture).

## Principes

1. **Natif intégral, 3 OS** — macOS (Swift/SwiftUI), Windows (C#/WinUI 3),
   Linux (GTK4/gtk-rs). Pas de webview / Electron / Tauri.
2. **Décentralisé & stateless** — aucun composant central obligatoire. Tout état
   vit dans le réseau ou en cache local. Bootstrap & relay sont **sans état** et
   multipliables par n'importe qui.

## Architecture

Un **noyau Rust unique** (`crates/champinium-core`, tokio + rust-libp2p) contient
toute la logique et est exposé aux fronts via **UniFFI**. Voir [`CLAUDE.md`](CLAUDE.md)
pour le détail (P2P, feeds signés, catalogue CRDT, modération, risques).

```
crates/champinium-core/   noyau Rust partagé (UniFFI) — TOUTE la logique
crates/champinium-cli/    outil debug
crates/bootstrap/         nœud rendez-vous stateless
crates/relay/             relay NAT stateless
apps/{macos,windows,linux}/  fronts natifs (présentation uniquement)
deny/                     denylist par défaut (modération)
```

## Build

Nécessite [`just`](https://github.com/casey/just) et la toolchain Rust.

```sh
just build-rust     # compile le workspace Rust
just check          # fmt + clippy + tests
just gen-swift      # bindings Swift  (macOS)  -> bindings/swift/
just gen-csharp     # bindings C#     (Windows)-> bindings/csharp/ (cargo install uniffi-bindgen-cs)
```

Les bindings générés ne sont **pas commités** (régénérés au build).

## Faire tourner sa propre infra (bootstrap / relay)

Bootstrap et relay sont **sans état** et conçus pour être multipliés. N'importe
qui peut lancer les siens : `cargo run --release -p champinium-bootstrap` /
`-p champinium-relay`. La procédure détaillée (adresses d'écoute, clés, exposition
réseau) sera documentée dans `docs/` à mesure que ces composants sont implémentés.

---

## ⚖️ Volet juridique — à lire (ce n'est PAS une note de bas de page)

Champinium est un **réseau P2P ouvert**. Cela a des conséquences juridiques
directes qui **contraignent le design** et que nous documentons franchement.

**Du contenu illégal circulera.** Sur un réseau ouvert et décentralisé,
n'importe qui peut publier n'importe quoi, et **la suppression centralisée d'un
contenu est impossible par construction** : il n'existe aucun serveur ni base que
nous (ou quiconque) pourrions purger. C'est une propriété structurelle, pas un
défaut corrigeable. Le choix d'**interopérer avec le réseau IPFS public** expose
en plus à du contenu provenant de pairs non-Champinium.

**Notre réponse de design : la modération côté nœud, active par défaut.**
- Hash-matching local contre des bases de contenus illégaux connus, **à
  l'ingestion** ET **à la réception avant tout reseed**.
- **Denylists signées** souscrites (modèle fédéré), avec une liste par défaut
  active à l'installation et **non désactivable** (voir [`deny/`](deny/)).
- Refus de seeder tout contenu matché ; procédure de signalement P2P.

Cela **réduit** la circulation de contenu connu comme illégal sur les nœuds
conformes ; cela **ne l'élimine pas** et ne peut pas l'éliminer.

**Responsabilités.**
- **Éditeur de l'application** : la distribution d'un logiciel facilitant le
  partage P2P engage potentiellement la responsabilité de son éditeur selon les
  juridictions (notamment dans le cadre européen — **DSA**, directives sur le
  droit d'auteur, obligations de moyens de modération). Le design intègre des
  garde-fous (modération par défaut non désactivable) précisément pour cette
  raison.
- **Opérateur de bootstrap / relay** : faire tourner un nœud bootstrap ou relay
  vous place dans le rôle d'**intermédiaire technique**. Selon votre juridiction
  et la nature exacte du service rendu, des obligations (réponse aux signalements,
  coopération avec les autorités) peuvent s'appliquer à votre **hébergeur** comme
  à vous-même. Ces nœuds sont **sans état** et ne stockent pas de contenu, mais
  facilitent la mise en relation.
- **Utilisateur / seeder** : reseeder du contenu, c'est le rediffuser. La
  responsabilité de ce que vous seedez vous incombe.

**Ceci n'est pas un avis juridique.** Avant tout déploiement public ou toute
opération de bootstrap/relay, consultez un conseil compétent dans votre
juridiction. Le projet fournit des outils de modération ; il ne vous décharge
d'aucune responsabilité légale.

## Licence

[Apache-2.0](LICENSE).
