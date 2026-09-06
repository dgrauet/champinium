# Déployer un bootstrap et/ou un relay (par n'importe qui)

Les deux pièces « centrales » résiduelles de Champinium sont **SANS ÉTAT** et
**multipliables trivialement** : n'importe qui peut en héberger, et le réseau
n'en dépend d'aucune en particulier. Elles ne stockent **aucun contenu** —
seule leur clé d'identité est persistée (pour offrir un PeerId/multiaddr stable
que des tiers peuvent référencer : c'est de la configuration, pas de l'état
réseau).

- **`champinium-bootstrap`** — point de rendez-vous Kademlia : aide les
  nouveaux nœuds à découvrir des pairs. Il ne sert jamais de blocs.
- **`champinium-relay`** — circuit relay v2 + assistance DCUtR : met en
  relation les nœuds derrière NAT (réservations + circuits), sans jamais voir
  le contenu en clair de bout en bout (connexions chiffrées Noise entre pairs).

## Build

```sh
cargo build --release -p champinium-bootstrap -p champinium-relay
# binaires : target/release/champinium-{bootstrap,relay}
```

## Bootstrap

```sh
champinium-bootstrap --listen /ip4/0.0.0.0/tcp/4101 --data-dir /var/lib/champinium-bootstrap
```

Sortie (smoke-testé) :

```
champinium-bootstrap en ligne (stateless)
PeerId : 12D3KooW…
Adresse: /ip4/0.0.0.0/tcp/4101/p2p/12D3KooW…
Référez ce multiaddr comme --bootstrap chez les autres nœuds.
```

Publiez le multiaddr **avec votre IP/nom public** :
`/ip4/<IP-publique>/tcp/4101/p2p/<PeerId>` (ou `/dns4/<hôte>/tcp/4101/p2p/…`).
Les nœuds l'utilisent via `champinium-cli serve --bootstrap <multiaddr>` (ou
`champinium-seed --bootstrap …`).

## Relay

```sh
champinium-relay --listen /ip4/0.0.0.0/tcp/4201 --data-dir /var/lib/champinium-relay
```

Sortie (smoke-testé) :

```
champinium-relay en ligne (stateless)
PeerId : 12D3KooW…
Adresse: /ip4/…/tcp/4201/p2p/12D3KooW…
Circuit : /ip4/…/tcp/4201/p2p/12D3KooW…/p2p-circuit
Nœuds NAT : écoutez sur <circuit>. Autres : dialez <circuit>/p2p/<peer-NAT>.
```

Usage côté nœuds :
- un nœud **derrière NAT** écoute sur l'adresse de circuit (`…/p2p-circuit`) —
  il obtient une réservation et devient joignable via le relais ;
- un pair le joint en dialant `…/p2p-circuit/p2p/<PeerId-du-nœud-NAT>` ; DCUtR
  tente ensuite un hole punching pour établir une connexion directe (le relais
  ne reste dans le chemin que si le hole punching échoue).

Le relais déclare son adresse d'écoute comme **adresse externe** au démarrage —
sans cela les réservations seraient acceptées sans adresse exploitable. Si le
relais est lui-même derrière un NAT avec redirection de port, exposez le port
TCP choisi (4201 par défaut).

## Prérequis réseau

| Pièce | Port par défaut | À ouvrir |
|---|---|---|
| bootstrap | 4101/tcp | entrant |
| relay | 4201/tcp | entrant |

Pas de base de données, pas de volume de contenu : le dimensionnement est
minimal (le relais consomme de la bande passante uniquement pour les circuits
dont le hole punching a échoué).

## Service systemd (Linux, exemple)

```ini
# /etc/systemd/system/champinium-bootstrap.service
[Unit]
Description=Champinium bootstrap node (stateless)
After=network-online.target
Wants=network-online.target

[Service]
DynamicUser=yes
StateDirectory=champinium-bootstrap
ExecStart=/usr/local/bin/champinium-bootstrap \
  --listen /ip4/0.0.0.0/tcp/4101 --data-dir /var/lib/champinium-bootstrap
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

Idem pour le relay en remplaçant le binaire, le port (4201) et le
`StateDirectory`. Sur macOS, s'inspirer de
[`infra/services/com.champinium.seed.plist`](../infra/services/README.md)
(launchd) en adaptant le binaire.

## Ce que l'opérateur héberge (et n'héberge pas)

- **Héberge** : un point de rendez-vous DHT et/ou un service de mise en
  relation NAT. La clé privée du service (`node.key`, mode 0600) est le seul
  fichier à sauvegarder si l'on veut garder un PeerId stable.
- **N'héberge pas** : de contenu. Aucun bloc n'est stocké ni servi par ces
  pièces ; la modération de contenu se joue sur les nœuds (checkpoints du
  noyau), pas ici.
- Volet juridique : voir le README du repo (responsabilité d'hébergeur,
  cadre DSA/UE) — un opérateur de bootstrap/relay fournit de la connectivité,
  pas du contenu.

## Être embarqué dans la liste par défaut (ADR 0013)

Un nœud lancé via `champinium-cli serve --bootstrap …` ou `--bootstrap` sur
`champinium-seed` ne compose que ponctuellement, sans persister l'adresse.
Pour qu'un bootstrap serve **tout nouveau nœud sans configuration**, il faut
l'ajouter à la liste compilée dans le binaire :
[`bootstrap/default.peers`](../bootstrap/default.peers) — vide aujourd'hui,
aucun bootstrap public n'existe encore.

Procédure : ouvrir une PR ajoutant une ligne `/…/p2p/<peerid>` à ce fichier,
avec dans la description un engagement de disponibilité (uptime visé,
contact opérateur). Le nœud doit écouter sur le port **4101/tcp** ci-dessus
et rester stateless. Voir [`bootstrap/README.md`](../bootstrap/README.md).

Cette liste **n'est pas signée** : contrairement à la denylist projet
(ADR 0011), l'intégrité repose sur la chaîne de confiance du binaire
lui-même (build reproductible, release signée), pas sur un mécanisme
cryptographique séparé — un bootstrap malveillant ne peut que refuser de
répondre ou fournir de faux pairs, jamais falsifier du contenu
(content-addressed, vérifié par CID). Publier en `/dns4/<hôte>/tcp/4101/p2p/…`
plutôt qu'une IP figée est préférable : le transport résout désormais les
noms d'hôte (ADR 0013).

## Mise à niveau — DHT dédiée (ADR 0012)

Depuis cette version, le protocole Kademlia du réseau est
`/champinium/kad/1.0.0` (auparavant celui, générique, d'IPFS public — voir
[`docs/adr/0012-dedicated-dht-and-root-providing.md`](adr/0012-dedicated-dht-and-root-providing.md)).
**C'est une rupture protocolaire dure** : un nœud sur l'ancien protocole et un
nœud sur le nouveau ne partagent plus aucune DHT — plus aucun `get_providers`
ni fetch de feed/denylist entre les deux, dans les deux sens. Le reste de la
pile (`identify`, topics gossipsub) est inchangé, donc les deux versions
continuent de se connecter et d'échanger des feeds par gossip : un ancien nœud
voit le catalogue d'un créateur récent se peupler normalement, mais **son
contenu n'est jamais récupérable** (aucun fournisseur découvrable) — un échec
silencieux, pas un rejet net.

**Mettez à jour bootstrap, relays et clients ensemble.** Un bootstrap ou un
relay resté sur l'ancien binaire continue de fonctionner comme point de
rendez-vous/relais générique (ces rôles ne dépendent pas du protocole Kademlia
applicatif), mais n'aide plus à la découverte de contenu pour les clients déjà
mis à jour : mettez-les à niveau en même temps que le reste du réseau plutôt
qu'en différé.
