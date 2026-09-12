# 0014 — Persistance de la longue traîne : seed de ce que je regarde, maintenance intégrée

- Statut : accepté
- Date : 2026-09-07

## Contexte

Depuis le retrait de seed-what-you-consume (spec channels lot c), la
persistance d'une publication dépend entièrement du seed proactif des
**abonnements** : `StorePolicy::Stream` par défaut ne laisse rien au
blockstore, et seule la boucle de seed sur les channels souscrits (quota,
éviction par réplication, pins) retient du contenu. Trois angles morts
restaient ouverts :

- **Un créateur sans abonné n'est répliqué nulle part.** La vue **Explorer**
  (catalogue complet, hors abonnements) ne génère aucune réplication : la
  regarder une fois ne laisse aucune trace, même quand l'utilisateur aurait
  volontiers aidé à seeder ce qu'il vient de voir.
- **`reprovide_all` et `republish_known_feeds` n'étaient appelés que par le
  démon `champinium-seed`**, séparé de l'application GUI. Un nœud ouvert par
  un front (FFI ou GTK) ne réannonçait jamais ses racines après un
  redémarrage : le store de providers Kademlia étant volatil, tout ce que ce
  nœud seedait devenait introuvable jusqu'à ce que le démon tourne — or rien
  n'installe le démon par défaut. Un utilisateur portable qui ferme
  simplement l'application cessait de servir, contrairement à ce que
  l'ADR 0007 (IPNS différé) supposait déjà couvert par la réannonce.
- **La réplication toutes-directions a été retirée à dessein** au lot (c) des
  channels ; cet ADR ne la réintroduit pas — voir « Conséquences ».

`open_stream` choisissait déjà `Seed` pour un manifeste d'un channel
souscrit ; à la complétion d'une telle session, `seed_wake` réveille
`seed_loop`, qui fait entrer la publication au `SeedIndex` via
`seed_publication` (les blocs sont déjà locaux, l'insertion ne coûte que la
comptabilité du quota). Le même mécanisme, étendu et borné, couvre la
longue traîne sans réintroduire de réplication non sollicitée.

## Décisions

1. **Maintenance intégrée au nœud.** Une boucle `maintenance_loop` dans
   `Node` (même patron que `seed_loop` : un `Weak` de vivacité, jamais un
   `Node` fort retenu par la tâche) appelle `reprovide_all()` puis
   `republish_known_feeds()` à son premier tour, puis toutes les
   `REPROVIDE_INTERVAL` (1 h). Elle démarre au **premier `listen` réussi**
   (flag `maintenance_started`, une seule fois par nœud, quel que soit son
   porteur) — un nœud qui n'écoute pas ne sert rien, et `Node::open`/`new`
   restent sans effet réseau implicite (les tests unitaires créent des
   nœuds sans réseau sans en subir d'effet de bord). Intervalle injectable
   via `with_moderation_and_intervals` (nouveau paramètre
   `reprovide_interval`, comme `seed_interval`) pour les tests. **La
   première passe réelle attend un premier pair connecté** (courtes
   tentatives toutes les 2 s tant qu'aucun pair n'est joignable) : tous les
   porteurs réels appellent `listen` avant de joindre le réseau
   (`--bootstrap`/`bootstrap()` pour le démon, `openNode → listen →
   connect` pour les fronts), donc une passe lancée immédiatement trouverait
   une table de routage vide, stockerait ses provider records localement
   sans qu'ils atteignent quiconque, et journaliserait un succès mensonger.
   Aucune passe ni aucun log de succès sans pair connecté.
2. **Démon simplifié.** `champinium-seed` ouvre, écoute, bootstrap, puis
   attend `ctrl_c` — la maintenance périodique est désormais celle du nœud.
   Le flag `--reprovide-interval` est **retiré des fichiers de service**
   fournis, mais reste **accepté et inerte** en ligne de commande (masqué de
   l'aide, `warn!` de dépréciation) : un fichier de service déjà déployé qui
   passe encore l'option continue de démarrer après une mise à jour du
   binaire, au lieu de boucler en échec sous systemd. Le démon garde sa
   seule raison d'être : **servir quand l'application est fermée**.
   `infra/services/README.md` le formule ainsi : « l'app seede tant qu'elle
   est ouverte ; le démon, quand elle est fermée ».
3. **Seed de ce que je regarde, opt-in.** Dotfile `.seed_watched` (défaut
   `false`), `Node::seed_watched() -> bool` / `set_seed_watched(bool)`
   (synchrones, effet immédiat, persisté). Activé, `open_stream` d'un
   manifeste **hors abonnement** prend `StorePolicy::Seed` si l'émetteur est
   identifiable au catalogue (une entrée dont `cids` contient le manifeste),
   `Stream` sinon (sans émetteur, pas d'entrée d'index possible). À la
   **complétion** de la session (tous les segments récupérés), la
   publication entre au `SeedIndex` sous cet émetteur, **non épinglée**, par
   `seed_publication` (quota + éviction) — déclenchée directement par la
   session via un nouveau canal `seed_now: mpsc<(PeerId, Cid)>` consommé par
   `seed_loop`, qui appelle `seed_publication` sans passer par le
   round-robin des abonnements. Si le quota ne peut pas faire de place, la
   publication est purgée (blocs orphelins retirés) plutôt que de rester
   hors index.
4. **Session incomplète.** À `close_stream` d'une session `Seed` d'un
   manifeste **hors abonnement** non encore indexé, les segments récupérés
   non référencés par l'index sont supprimés (`remove_unshared_blocks`) —
   pas d'orphelins qui grossissent hors quota. Pour un manifeste
   **souscrit**, comportement inchangé (le seed proactif complète plus
   tard).
5. **Éviction à deux étages.** `eviction_order` gagne un ensemble
   `subscribed: &HashSet<String>` (émetteurs souscrits) ; les publications
   d'émetteurs **non souscrits** partent en premier (réplication
   décroissante puis âge, comme aujourd'hui, à l'intérieur de chaque étage).
   Un abonnement est une intention explicite ; un visionnage, une
   opportunité. L'inégalité stricte anti-oscillation (le dampener existant)
   est conservée dans chaque étage.
6. **Désabonnement.** `purge_issuer` reste inchangé dans sa logique (purge
   les publications non épinglées de l'émetteur retiré) — si `seed_watched`
   est actif, les publications regardées de cet émetteur partent aussi
   (elles ne sont jamais épinglées) : simple et prévisible, documenté plutôt
   que traité en cas spécial.
7. **Blocage / modération.** `purge_blocked_issuer` couvre déjà l'index
   entier, pins compris — rien à changer ; le checkpoint #2 s'applique aux
   sessions `Seed` déclenchées par le seed de ce que je regarde exactement
   comme à celles des abonnements.
8. **Observabilité.** `FfiCatalogEntry.seeded_count` reflétait déjà les
   publications indexées quel que soit l'abonnement ; la vue Explorer
   affiche « conservé » sur une entrée non souscrite dont `seeded_count > 0`.
9. **Contrat FFI v15.** `seed_watched() -> bool` / `set_seed_watched(enabled)`
   (sync). Fronts : case « Conserver et resservir ce que je regarde » dans le
   volet réglages de seed, légende « Hors abonnement, sous le même quota ;
   évincé avant les abonnements. », même patron sans écriture au chargement
   que la case mDNS (ADR 0013). CLI : `seed-watched [--set on|off]`.

## Contrat FFI

**v15** : `seed_watched() -> bool` / `set_seed_watched(enabled: bool)` (sync,
persiste dans `.seed_watched`, défaut `false`, effet **immédiat** —
contrairement à `set_mdns` de l'ADR 0013 qui n'agit qu'au prochain
démarrage, car ici seule la politique de stockage des futures sessions de
lecture change, pas la construction du swarm). Voir
[`AGENTS.md`](../../AGENTS.md).

## Conséquences

- **Un nœud GUI redémarré resert ce qu'il détient sans qu'un démon tourne** :
  la maintenance vit dans le nœud, démarrée par `listen`, quel que soit son
  porteur (front, CLI, démon), **première passe réelle dès qu'un premier
  pair est connecté** (bootstrap, mDNS ou connexion manuelle) — jamais
  immédiatement sur une table de routage vide. C'était l'écart le plus net
  avec ce que l'ADR 0007 supposait déjà acquis.
- **Toute commande CLI ou binaire qui appelle `listen` déclenche une passe de
  réannonce immédiate de son propre blockstore** — `champinium-cli serve`,
  `champinium-bootstrap`, `champinium-seed`, ou toute commande one-shot qui
  ouvre puis écoute. C'est voulu : la maintenance ne distingue pas les
  porteurs du nœud, et une commande de debug qui écoute brièvement réannonce
  ce qu'elle détient exactement comme un front GUI le ferait.
- **Le seed de ce que je regarde partage le quota des abonnements** plutôt
  que d'en ouvrir un séparé — un seul budget à comprendre pour
  l'utilisateur — mais **cède toujours la place** : l'éviction à deux étages
  garantit qu'une publication seedée par simple visionnage ne déloge jamais
  une publication d'un channel souscrit, quelle que soit sa réplication
  respective.
- **Le démon `champinium-seed` devient un filet de continuité plutôt que le
  porteur de la maintenance** : sa présence n'est plus nécessaire pour
  qu'un nœud réannonce correctement ses racines pendant qu'il tourne dans un
  front — seulement pour continuer à servir une fois l'application fermée.
  Les fichiers de service dans `infra/services/` restent recommandés pour
  cet usage.
- **Pas de réplication toutes-directions.** Le seed de ce que je regarde
  reste strictement déclenché par une **lecture volontaire** de
  l'utilisateur (`open_stream`), jamais par une passe de fond qui
  parcourrait le catalogue pour répliquer ce que d'autres nœuds
  sous-répliquent. La réplication toutes-directions retirée au lot (c) des
  channels (`replicate_under_provided` et les flags de démon associés)
  **n'est pas réintroduite** par cet ADR — sa réouverture reste conditionnée
  à une décision explicite de spec, jamais à une extension incrémentale d'un
  mécanisme voisin.

## Limites

- **Lire par CID hors catalogue n'est pas retenu.** Le seed de ce que je
  regarde exige un émetteur identifiable au catalogue pour construire une
  entrée d'index (`SeedIndex` indexe par émetteur) ; une lecture d'un CID de
  manifeste obtenu hors catalogue (lien direct, CLI `get --root`) reste en
  `Stream` même l'option activée. Assumé : documenter au lieu d'étendre
  `SeedIndex` à un mode sans émetteur, qui aurait cassé l'hypothèse
  d'indexation par éditeur utilisée par le désabonnement et la modération
  par clé.
- **Le désabonnement purge aussi les visionnages retenus** de l'émetteur
  retiré, pas seulement les publications entrées via le seed proactif des
  abonnements — un utilisateur qui se désabonne après avoir laissé le seed
  de ce que je regarde conserver du contenu de cet émetteur le perd aussi.
  Décision 6, assumée pour rester prévisible plutôt que d'introduire une
  distinction fine entre origines d'indexation.
- **Un manifeste regardé mais explicitement épinglé** (`pin_content` avant ou
  pendant la lecture) est conservé sans jamais entrer à l'index de seed ni
  compter dans son quota affiché — cohérent avec le comportement existant de
  `pin_content`, mais crée un écart entre l'occupation disque réelle et le
  quota affiché aux fronts. Pas nouveau à cet ADR (déjà vrai pour les pins
  d'abonnement), simplement plus visible avec un chemin d'entrée
  supplémentaire.
- **Pas de balayage d'orphelins au démarrage.** La purge à `close_stream`
  (décision 4) couvre le cas « l'utilisateur ferme la lecture avant la fin » ;
  elle ne couvre pas un kill brutal du process entre la complétion des
  segments et l'entrée effective au `SeedIndex` (fenêtre étroite, entre la
  fin du transfert et le traitement de `seed_now` par `seed_loop`). Même
  dette pour un désabonnement survenant pendant une session `Seed` de
  l'émetteur retiré : `open_stream` avait mis cette session hors index
  (`issuer_to_index = None`, comportement des abonnements, décision 4), donc
  sa fermeture ne purge rien et les segments déjà récupérés restent au
  magasin, hors index et hors quota, jusqu'au prochain seed ou à un futur
  balayage. Dette assumée : un balayage d'orphelins au démarrage du
  blockstore est une tâche dédiée à venir, hors du périmètre de cet ADR.
