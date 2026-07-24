# 0008 — Stockage froid optionnel : Arweave, payé par le créateur, découverte par tags CID

- Statut : accepté — **lot CS-a implémenté** (voir note de statut ci-dessous) ;
  lot CS-b (fronts, contrat FFI v10) non lancé
- Date : 2026-07-23 (note de statut : 2026-07-24)

> **Note de statut — CS-a implémenté (2026-07-24).** Le cœur et la CLI livrent
> le stockage froid derrière la feature cargo opt-in **`cold-storage`** (absente
> des builds par défaut ; aucune dépendance crypto/HTTP dans le graphe par
> défaut, d'où l'absence d'ignore CVE dans `deny.toml`) : trait `ColdStore` +
> backend `ArweaveColdStore` (signature hand-roll deep-hash + RSA-PSS), repli
> CID-vérifié dans `Node::get_with` (débrayable), archivage devis→confirmation
> payé par le créateur, CLI `archive`/`archives`/`cold-retrieval`, test
> d'intégration Arweave réel `#[ignore]`+env-gaté (`CHAMPINIUM_ARWEAVE_IT`).
> Réserves à reprendre :
> - **`rsa` 0.10-rc** : dépendance de signature en **pré-release** (contenue par
>   le feature-gating) — à repasser sur une version stable dès sa publication.
> - **Upload inline** : la transaction est POSTée en une fois (`POST /tx`, data
>   inline dans la transaction format 2) — suffisant pour manifeste et petits
>   segments, mais **l'upload chunké (`/chunk`) reste à faire pour la vidéo**
>   (segments au-delà de la limite inline d'Arweave).
> - **Forme par item-tx** : **une transaction par item** (manifeste + chaque
>   segment), et non « une transaction/bundle » comme l'écrit la décision §3
>   ci-dessous. C'est **imposé par la récupération par CID** (§5 : chaque octet
>   récupéré est vérifié seul contre son CID) — écart délibéré et correct ; la
>   décision §3 est à lire à travers cette note.

## Contexte

Le risque #1 du projet est la **persistance** : depuis la refonte channels
(lot c), un nœud ne sème que ce qu'il a souscrit — la survie d'un contenu
dépend donc uniquement de ses abonnés, et un contenu sans abonné disparaît
quand son créateur s'éteint. C'est un choix assumé (« persiste ce que des gens
ont choisi de porter »), mais il laisse un vide pour le créateur qui tient à
la pérennité de ses publications au-delà de son audience du moment. Le
stockage froid (Filecoin/Arweave) était jusqu'ici « documenté, non
implémenté » en trois endroits sans position argumentée. Cet ADR fixe la
position.

## Décision

1. **Optionnel, déclenché et payé par le créateur, pour son propre contenu.**
   Jamais automatique, jamais silencieusement payant : toute action
   d'archivage affiche un devis (taille, coût estimé en AR, solde du
   portefeuille) et exige une confirmation explicite. Le portefeuille est
   apporté par le créateur (fichier JWK Arweave référencé par chemin,
   permissions 0600) — champinium ne crée, ne gère ni ne finance rien.
2. **Arweave d'abord, derrière un trait `ColdStore`.** Arweave = paiement
   unique pour un stockage à visée permanente (modèle d'endowment), aucune
   obligation de renouvellement — cohérent avec le principe « stateless au
   maximum » (un deal Filecoin exige un processus vivant qui surveille et
   repaie, exactement l'état permanent que le projet refuse). Le trait isole
   le backend : Filecoin pourra s'ajouter si les volumes le justifient, sans
   toucher ni à l'archivage ni au repli.
3. **Unité d'archivage = la publication** (manifeste HLS + segments — l'unité
   du SeedIndex et des pins), en une transaction/bundle dont chaque item est
   étiqueté `champinium-cid: <cid>` (+ `champinium-schema: hls/v1`).
4. **Découverte de l'archive par recherche déterministe de tags** (GraphQL des
   gateways, liste configurable, plusieurs par défaut) — aucun changement de
   format de feed, et un contenu archivé est retrouvable même si l'archiveur
   ne l'a signalé nulle part.
5. **Récupération = repli, jamais chemin principal.** Uniquement quand le P2P
   conclut à `NoProviders`. Tout octet récupéré est **vérifié contre son CID**
   avant toute utilisation — les gateways ne sont jamais de confiance (elles
   peuvent servir du silence, jamais du faux). Ensuite le flux normal
   s'applique : politique Seed/Stream inchangée (souscrit → le contenu
   ré-entre au SeedIndex et **réamorce le réseau P2P** ; sinon lecture sans
   trace), et le checkpoint de modération #2 inchangé — l'archive ne
   contourne aucune modération, et les clés bannies restent invisibles (feeds
   rejetés en amont).
6. **Repli débrayable** (réglage « Récupération d'archive », actif par
   défaut) : interroger une gateway par CID révèle à cette gateway l'intérêt
   de l'IP pour ce CID — surface d'observation différente du P2P pur,
   documentée avec la même franchise que l'observabilité DHT du suivi actif.

## Conséquences

- Le vide « contenu sans abonné = perte définitive » a une réponse opt-in,
  sans imposer de coût à personne et sans composant central : l'archive est
  un filet de dernier recours qui peut réamorcer le réseau.
- La *découverte* dépend de l'indexation des tags par les gateways ;
  l'*intégrité* n'en dépend jamais (vérification CID). Un CID que plus
  personne ne connaît reste introuvable — le feed durable du créateur
  (republication par les abonnés, cf. note de l'ADR 0007) reste le chemin de
  découverte.
- Ordre de grandeur des coûts au moment de la rédaction : de l'ordre de
  quelques dollars à une dizaine de dollars par Go archivé selon le cours de
  l'AR — significatif pour de la vidéo, d'où l'archivage par publication
  choisie et non « tout mon channel » en un clic. À re-vérifier au lancement
  du lot CS-a (le devis, lui, est toujours calculé en temps réel).
- La permanence est une promesse économique du réseau Arweave, pas une
  garantie de champinium — documentée sans sur-vente.

## Phasage (différé — chacun son plan au lancement)

- **CS-a — cœur + CLI** : trait `ColdStore`, backend Arweave
  (archive/retrieve/prix), repli dans `get_with`, CLI `archive`/`archives`.
  Tests contre gateway mock ; test d'intégration réel optionnel gaté par
  variable d'environnement (coûte de vrais AR). À trancher à ce moment-là :
  lib Rust Arweave existante vs implémentation directe du format de
  transaction.
- **CS-b — fronts** : bouton « Archiver » (dialogue de devis), liste « mes
  archives » (reçus locaux `.archives`, statut de confirmation re-interrogé à
  la demande — pas de démon), réglage du repli. Contrat FFI v10.

## Références

- Spec de design détaillée (artefact local, hors repo) :
  `~/Work/.superpowers/champinium/specs/2026-07-23-cold-storage-design.md`.
- ADR 0007 (IPNS différé) — même famille de décisions sur la durabilité.
