# 0015 — Signalements pondérés par les clés de confiance

- Statut : accepté
- Date : 2026-09-12

## Contexte

Le signalement P2P (Phase 5) fait émettre au nœud un rapport signé
`champinium-report/v1` quand le checkpoint #2 refuse un contenu ; chaque pair
agrège localement le nombre de **rapporteurs distincts** par CID dans un
`ReportBook` borné (10 000 CIDs, 1 000 rapporteurs/CID, refus-quand-plein).
C'est de la matière première pour les éditeurs de denylists (modération
fédérée), sans effet automatique.

Les identités Ed25519 étant gratuites (aucun coût à en générer une nouvelle),
ce compteur brut est trivialement gonflable par sybil : quiconque peut faire
grimper le nombre de « rapporteurs distincts » d'un CID avec autant de clés
jetables qu'il veut, ou remplir le livre (10 000 CIDs) pour que des rapports
légitimes suivants soient refusés faute de place. Un compteur brut
n'informe donc pas un éditeur de denylist, il l'expose à être manipulé — un
adversaire peut faire bannir le contenu d'un tiers, ou saturer le mécanisme
pour noyer les signaux réels.

Le nœud connaît déjà, localement, un ensemble de clés choisies explicitement
par l'utilisateur : les **éditeurs de denylist souscrits**
(`Moderation::sources`, éditeur projet toujours présent en tête,
[ADR 0011](0011-reputational-moderation.md)). Cet ensemble est petit, déjà
responsable de listes signées, et ne coûte rien de plus à réutiliser comme
socle de confiance pour les signalements.

## Décisions

1. **Deux étages de rapporteurs.** `ReportBook` sépare les rapporteurs **de
   confiance** (dont le PeerId appartient à l'ensemble des éditeurs de
   denylist souscrits) des **autres**. Chaque compteur devient
   `ReportTally { trusted: usize, others: usize }` (+ `total()`, à ne lire
   qu'avec `trusted` sous les yeux — seule cette colonne résiste aux clés
   jetables).
2. **Bornes séparées.** L'étage de confiance a sa propre borne
   (`MAX_TRUSTED_REPORTED_CIDS = 10 000`) et **aucune borne par CID** — le
   nombre de clés de confiance est petit et choisi par l'utilisateur, il ne
   peut pas noyer un CID donné. L'étage « autres » garde les bornes
   historiques (`DEFAULT_MAX_REPORTED_CIDS = 10 000`,
   `DEFAULT_MAX_REPORTERS_PER_CID = 1 000`). Chez le nœud qui a souscrit le
   rapporteur, un rapport de confiance n'est donc jamais refusé parce que
   l'étage « autres » est plein, ni l'inverse — cette garantie est **locale**,
   voir « Limites ».
3. **Reclassement à chaud.** À `subscribe_denylist_issuer` /
   `unsubscribe_denylist_issuer` (et au chemin `subscribe_denylist` qui
   inscrit lui aussi un éditeur dans les issuers souscrits),
   `ReportBook::retrust(&trusted)` déplace les rapporteurs déjà connus entre
   étages selon le nouvel ensemble de confiance. Le livre naît avec
   l'ensemble de confiance déjà à jour (construit après le chargement des
   éditeurs souscrits persistés, éditeur projet compris) — jamais une
   fenêtre où un rapport d'un éditeur souscrit tomberait à l'étage
   « autres ». Le livre lui-même n'est **pas persisté** (inchangé) : il
   repart vide à chaque démarrage et se reconstruit par gossip.
4. **Lecture.** `Node::report_count(cid) -> ReportTally`,
   `report_counts() -> Vec<(Cid, ReportTally)>`,
   `report_counts_by_channel() -> Vec<(PeerId, ReportTally, u64)>` (le
   cumul additionne les rapporteurs par CID de l'émetteur — un volume de
   signalements, pas un nombre de personnes ; un CID revendiqué par
   plusieurs émetteurs n'est attribué à aucun, voir « Limites »). CLI `reports [--by-channel]
   [--all]` affiche « N de confiance / M autres », trie par `trusted`
   décroissant puis `others` décroissant, et ne liste **par défaut** que les
   CIDs avec `trusted ≥ 1` — un compteur « autres » seul n'est pas affiché
   sans `--all`, précisément pour qu'un millier de clés jetables ne change
   pas ce qui remonte en premier.
5. **Émission inchangée.** Le nœud continue d'émettre un rapport signé au
   refus du checkpoint #2, quelle que soit sa propre « confiance » aux yeux
   des autres pairs.
6. **Pas de FFI, pas de fronts.** Aucun compteur de signalements n'est
   ajouté à la surface UniFFI ni affiché dans les trois fronts (contrat v15
   inchangé) — le signalement reste un outil d'éditeur de denylist, pas une
   information utilisateur.
7. **Pas de pondération continue (réputation).** Une échelle de confiance
   graduée (score de réputation par pair, décroissance dans le temps, etc.)
   aurait exigé un mécanisme de calcul et de diffusion propre, avec ses
   propres angles d'attaque (gonfler sa propre réputation coûte alors
   moins cher que gonfler un compteur de rapports). Réutiliser un ensemble
   binaire déjà choisi explicitement par l'utilisateur — les éditeurs de
   denylist souscrits — évite d'introduire un second système de confiance
   à côté de celui de l'ADR 0011.

## Conséquences

- **Deux bornes indépendantes** au lieu d'une : dans le livre d'un nœud
  donné, l'étage de confiance ne peut jamais être évincé par l'étage
  « autres » et réciproquement.
- **Les signalements émis par le nœud lui-même tombent à l'étage « autres »** :
  sa propre clé n'est pas un éditeur de denylist qu'il aurait souscrit. Avec
  le filtre par défaut (`trusted ≥ 1`), un opérateur qui inspecte sa machine
  après un blocage ne voit donc pas ses propres refus sans `reports --all` ;
  l'aide de ce drapeau le dit.
- **L'ensemble de confiance est celui déjà choisi pour la modération**
  (ADR 0011), pas un nouveau registre — un utilisateur qui suit déjà des
  éditeurs de denylist bénéficie immédiatement de signalements filtrés sans
  configuration supplémentaire.
- **Aucun changement de surface utilisateur** : contrat FFI v15 inchangé,
  aucun front n'affiche de compteur de signalements — c'est un outil réservé
  aux éditeurs de denylist via le CLI.
- **Le livre reste volatil** (non persisté) : un nœud qui redémarre reperd
  tous les rapports reçus et les reconstruit par gossip au fil de l'eau,
  avec l'ensemble de confiance déjà correct dès la première insertion.

## Limites

- **Subjectivité locale.** L'ensemble de confiance est propre à chaque nœud
  (ses propres abonnements de denylist) — deux nœuds voisins peuvent
  classer le même rapporteur différemment. Ce n'est pas un défaut à
  corriger : c'est la même logique fédérée que l'ADR 0011, assumée ici pour
  les signalements comme pour les listes elles-mêmes.
- **Un éditeur souscrit qui devient hostile reste « de confiance »** tant
  qu'il reste souscrit — ses rapports comptent à l'étage de confiance même
  s'ils sont de mauvaise foi. Se désabonner de cet éditeur (
  `unsubscribe_denylist_issuer`) reclasse immédiatement ses rapports déjà
  connus vers l'étage « autres » via `retrust`, mais la fenêtre entre la
  dérive de l'éditeur et le désabonnement effectif de l'utilisateur reste
  entièrement à la charge de ce dernier — aucune détection automatique de
  dérive n'est prévue.
- **Le livre n'est pas persisté** : la classification de confiance ne
  survit pas à un redémarrage tant que les rapports concernés n'ont pas été
  regossipés — cohérent avec le choix historique de ne pas persister
  `ReportBook`, mais signifie qu'un nœud qui vient de redémarrer part avec
  un décompte à zéro même pour des rapporteurs de confiance déjà vus avant
  l'arrêt.
- **L'étage de confiance protège la lecture LOCALE, pas la propagation.**
  La garantie « un rapport de confiance n'est jamais refusé parce que
  l'étage “autres” est plein » ne vaut que chez le nœud qui a choisi de
  souscrire ce rapporteur. Sur un pair tiers quelconque du mesh, qui n'a
  aucune raison de suivre le même éditeur, ce rapport tombe à l'étage
  « autres » ; si un attaquant y a déjà déposé 10 000 CIDs jetables,
  l'insertion est refusée, le rapport n'est ni agrégé ni relayé
  (`MessageAcceptance::Ignore` — comportement antérieur à cet ADR et correct :
  ni relais, ni pénalité de score). La seconde attaque du contexte (remplir
  le livre) est donc neutralisée **à la lecture chez les nœuds concernés**,
  pas dans la propagation : le remplissage reste un moyen d'étouffer des
  signalements légitimes à l'échelle du réseau.
- **Un seul éditeur souscrit devenu hostile peut saturer l'étage de confiance
  pour les autres.** La borne `MAX_TRUSTED_REPORTED_CIDS` est **globale** à
  l'étage, pas par éditeur : un éditeur souscrit peut signaler à lui seul
  10 000 CIDs arbitraires et, l'étage étant en refus-quand-plein sans
  éviction, empêcher tout autre éditeur de confiance d'y faire entrer un CID
  nouveau — jusqu'au désabonnement, qui libère ses entrées via `retrust`. Le
  pouvoir de nuisance reste inférieur à ce que le même éditeur peut déjà
  faire via ses `key_entries`. Une borne par éditeur serait l'alternative si
  le besoin apparaît.
- **Un CID revendiqué par plusieurs émetteurs n'est attribué à aucun.**
  `report_counts_by_channel` joint les rapports au catalogue local, or lister
  un CID dans son feed signé ne prouve rien sur sa propriété — c'est
  l'invariant anti-censure du lot (d) / [ADR 0011](0011-reputational-moderation.md).
  Désigner un gagnant arbitraire permettrait à un publieur hostile de voler
  les signalements d'un tiers, et donc de **blanchir** un channel réellement
  problématique. La colonne « de confiance » par channel ne compte donc que
  les CIDs revendiqués par un **seul** émetteur ; les CIDs contestés, comme
  ceux absents du catalogue local, restent comptés au seul agrégat global
  par CID.
- **Aucun mécanisme n'empêche un éditeur de denylist de rapporter de mauvaise
  foi** un CID qui n'est pas le sien — la nuance anti-censure de l'ADR 0011
  (aucune liste de CIDs dérivée des feeds) protège le contenu lui-même, pas
  le compteur de signalements : un éditeur souscrit peut signaler n'importe
  quel CID, à charge pour l'utilisateur qui construit sa propre denylist de
  vérifier avant de bannir par clé.

## Contrat FFI

Inchangé, v15. Aucune méthode, aucun type et aucun callback UniFFI n'est
ajouté par cet ADR — voir [`AGENTS.md`](../../AGENTS.md).
