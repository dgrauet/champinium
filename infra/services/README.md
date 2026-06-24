# Seeding en arrière-plan (hors UI)

Le nœud seede même sans interface ouverte, via le démon **`champinium-seed`** :
au démarrage puis périodiquement, il réannonce dans la DHT tous les CIDs qu'il
détient (provider records) et republie son feed signé. Modèle
*seed-what-you-consume*. La modération par défaut reste active (un seeder ne
ressert jamais un contenu matché).

```sh
champinium-seed --data-dir <dir> [--listen <multiaddr>] \
    [--bootstrap <multiaddr> ...] [--reprovide-interval <secondes>]
```

Chaque OS l'enveloppe dans son gestionnaire de service natif.

## macOS — launchd

Fichier : [`com.champinium.seed.plist`](com.champinium.seed.plist).

```sh
cp com.champinium.seed.plist ~/Library/LaunchAgents/
# adapter les chemins (binaire, --data-dir)
launchctl load ~/Library/LaunchAgents/com.champinium.seed.plist
```

## Linux — systemd user service

Fichier : [`champinium-seed.service`](champinium-seed.service).

```sh
mkdir -p ~/.config/systemd/user
cp champinium-seed.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now champinium-seed.service
loginctl enable-linger "$USER"   # seeder même hors session
```

## Windows — Service

Pas de fichier déclaratif standard ; deux options :

```bat
:: Service natif (chemins absolus requis)
sc.exe create ChampiniumSeed binPath= "C:\Program Files\Champinium\champinium-seed.exe --data-dir C:\ProgramData\Champinium" start= auto
sc.exe start ChampiniumSeed
```

ou, plus robuste, via [NSSM](https://nssm.cc/) :

```bat
nssm install ChampiniumSeed "C:\Program Files\Champinium\champinium-seed.exe" --data-dir C:\ProgramData\Champinium
nssm start ChampiniumSeed
```

> Le binaire `champinium-seed` est commun aux trois OS (noyau Rust partagé) ;
> seul l'enrobage de service diffère.
