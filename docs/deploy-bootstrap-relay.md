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
