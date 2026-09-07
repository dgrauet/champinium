# 0013 — Découverte initiale : bootstraps embarqués, mDNS, DNS

- Statut : accepté
- Date : 2026-09-07

## Contexte

Le cœur ne connaissait aucun pair au démarrage : les trois fronts affichaient
un champ `/ip4/…/tcp/…/p2p/<peerid>` et un bouton « Connecter », le CLI et le
démon de seed prenaient `--bootstrap`, mais rien n'appelait
`kademlia.bootstrap()`. Deux nœuds sur le même réseau local ne se trouvaient
pas sans qu'un humain colle une adresse. Le transport n'acceptait pas les
multiaddrs `/dns4/…` : impossible de référencer un bootstrap public par nom
d'hôte.

Aucun bootstrap public n'est déployé aujourd'hui. Comme pour la clé projet de
modération (ADR 0011), le mécanisme doit exister avant que la première
adresse existe : une release suffira ensuite à remplir la liste, sans
changement de comportement pour les nœuds déjà installés.

## Décisions

1. **Liste de bootstraps compilée** — `bootstrap/default.peers` (une
   multiaddr `/…/p2p/<peerid>` par ligne, `#` = commentaire), compilée par
   `include_str!`. **Non signée** : le binaire lui-même est la confiance,
   comme pour la clé projet de modération. Vide au départ, avec un exemple
   commenté en `/dns4/…`. Voir [`bootstrap/README.md`](../../bootstrap/README.md)
   pour la procédure de proposition d'un bootstrap public.
2. **Bootstraps persistés par l'utilisateur** — dotfile `.bootstraps` à côté
   des blocs (JSON `Vec<String>`, même patron que `.subscriptions`) :
   multiaddrs ajoutées via l'API ou la CLI. L'union compilé ∪ persisté est la
   liste effective (`Node::bootstraps`) ; il n'existe pas de retrait des
   entrées compilées. Borne cumulée `MAX_BOOTSTRAPS = 64`.
3. **`Node::bootstrap()`** — compose vers toutes les adresses de la liste
   effective (best-effort : `add_address` + `dial`, échecs journalisés, pas
   d'erreur propagée par adresse individuelle), puis déclenche
   `kademlia.bootstrap()` pour peupler la table de routage.
   `NoKnownPeers` (liste effective vide) est journalisé, jamais renvoyé en
   erreur. Renvoie le nombre de bootstraps dont le dial a été accepté.
   **Pas d'appel automatique par `Node::open`/`new`** : c'est aux fronts et
   aux démons de l'appeler après `listen`, pour garder la construction d'un
   `Node` sans effet réseau implicite (les tests unitaires créent des nœuds
   sans réseau).
4. **mDNS** — `libp2p::mdns::tokio::Behaviour` intégré au `Behaviour` via
   `Toggle`. Un pair découvert est ajouté à la table Kademlia et composé,
   best-effort (dial ciblé sur le `PeerId` découvert, pas une adresse nue).
   **Actif par défaut pour `Node::open`** (fronts, CLI), débrayable par
   dotfile `.mdns_enabled` — au même titre que le repli de récupération
   froide (ADR 0008), car il révèle la présence de ce nœud sur le réseau
   local. `Node::new`/`with_moderation*` (donc `champinium-seed` et
   `champinium-bootstrap`) démarrent, eux, mDNS **désactivé** sauf si
   `.mdns_enabled` dit explicitement `true` : un nœud construit par ces
   chemins-là (tests unitaires compris) ne doit ni révéler sa présence sur le
   LAN ni composer vers des nœuds étrangers sans qu'on le lui demande.
   Réglage exposé aux fronts ; effectif **au prochain démarrage** (le
   `Toggle` est construit une fois pour toutes à l'ouverture du swarm).
5. **DNS** — transport avec résolution (`with_dns()` du `SwarmBuilder`,
   feature `dns` de libp2p) pour accepter `/dns4/`, `/dns6/`, `/dnsaddr/` en
   plus de `/ip4/`, `/ip6/`.
6. **Observabilité** — `Node::connected_peers() -> usize` (pairs
   effectivement connectés) et `Node::bootstraps() -> Vec<Multiaddr>` (liste
   effective compilée ∪ persistée), affichés par les fronts comme
   « réseau : n pair(s) ».
7. **Fronts** — appellent `bootstrap()` en tâche de fond au démarrage, après
   `listen`. Le champ « Connecter » manuel reste (opérateurs, tests, cas où
   la découverte automatique ne suffit pas). Volet réglages : interrupteur
   « Découverte sur le réseau local (mDNS) » à côté du compteur de pairs.
8. **CLI / démons** — `--bootstrap` reste (ajout ponctuel, non persisté) ;
   `serve`, `champinium-seed` et `champinium-bootstrap` appellent
   `node.bootstrap()` après `listen` et impriment le nombre de dials lancés ;
   nouvelle commande `bootstraps [--add <multiaddr>]`.

## Contrat FFI

**v14** : `bootstrap() -> u32` (async), `connected_peers() -> u32` (async),
`add_bootstrap(multiaddr)` (async, `InvalidInput` sans `/p2p/`) ;
`bootstraps() -> Vec<String>` (sync), `mdns_enabled() -> bool` /
`set_mdns(enabled)` (sync, effet au prochain démarrage — documenté côté FFI).
Voir [`AGENTS.md`](../../AGENTS.md).

## Conséquences

- **La liste embarquée reste vide tant qu'aucun bootstrap public n'est
  publié.** Sans bootstrap connu (ni compilé ni ajouté par l'utilisateur) et
  sans pair mDNS sur le même LAN, un nœud neuf reste isolé — comportement
  inchangé par rapport à avant cet ADR dans ce cas précis, seule la
  mécanique de composition automatique existe désormais.
- **mDNS rend ce nœud visible sur le réseau local** (multicast, requête
  `_p2p._udp` standard) : n'importe quel autre appareil du même LAN peut voir
  qu'un nœud Champinium tourne à telle adresse, débrayable par réglage.
  Documenté avec la même franchise que le suivi actif des abonnements
  (`docs/architecture.md` §6) ou le repli de récupération froide (ADR 0008).
- **`/dns4/`, `/dns6/`, `/dnsaddr/` sont désormais résolus** dans les
  multiaddrs de dial (bootstrap compilé, persisté, ou passé en `--bootstrap`
  / `add_bootstrap`) : un opérateur peut publier un nom d'hôte stable plutôt
  qu'une IP figée.
- **Liste de bootstraps non signée** : contrairement à la denylist projet
  (ADR 0011), l'intégrité de `bootstrap/default.peers` repose uniquement sur
  la chaîne de confiance du binaire (build reproductible, release signée) —
  un binaire recompilé sans cette liste, ou avec une liste modifiée, reste
  possible. Un bootstrap malveillant ne peut cependant que refuser de
  répondre ou fournir de faux pairs Kademlia (le contenu reste
  content-addressed et vérifié par CID, la modération et les signatures de
  feed restent inchangées) : le risque est un déni de découverte, pas une
  compromission de contenu.
- **`deny.toml` gagne deux entrées** (`RUSTSEC-2026-0118`,
  `RUSTSEC-2026-0119`, `hickory-proto` 0.25.x transitif via
  `libp2p-dns`/`libp2p-mdns` 0.44/0.48) : boucles de décodage DNS non bornées
  et amplification CPU côté encodage, atteignables seulement si un
  attaquant contrôle déjà la réponse DNS reçue par ce nœud lors d'un dial
  `/dns4/`, ou si ce nœud encodait des réponses DNS (il n'en émet jamais,
  résolveur client uniquement) — surface DoS côté client, pas de compromission.
  Aucune version corrigée de `hickory-resolver`/`hickory-proto` n'est
  compatible avec `libp2p-dns` 0.44 sur libp2p 0.56 à ce jour. À lever dès
  que `libp2p-dns` bumpe vers hickory 0.26+.
- Le test mDNS deux-nœuds est marqué `#[ignore]` : le multicast est bloqué
  sur les runners CI (macOS/Windows) et souvent sur les machines de
  développement en sandbox. Validation manuelle deux-machines documentée
  dans [`docs/gui-demo.md`](../gui-demo.md).

## Limites

- Un opérateur qui veut être embarqué dans `default.peers` s'engage à une
  disponibilité que le projet ne peut pas garantir ni surveiller : c'est un
  engagement déclaratif (voir `bootstrap/README.md`), pas une SLA vérifiée.
- La borne `MAX_BOOTSTRAPS = 64` protège `add_bootstrap` contre un
  gonflement local du dotfile, pas contre une liste compilée déraisonnable —
  ce cas relève de la revue de PR sur `bootstrap/default.peers`.
- `set_mdns` n'a d'effet qu'au prochain démarrage (le comportement mDNS du
  swarm est construit une fois pour toutes à l'ouverture) : un utilisateur
  qui désactive mDNS reste visible jusqu'au redémarrage suivant.
