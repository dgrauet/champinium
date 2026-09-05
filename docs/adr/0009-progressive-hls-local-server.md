# 0009 — Lecture progressive par serveur HLS local

- Statut : accepté
- Date : 2026-09-05

## Contexte

`fetch_hls` téléchargeait tous les segments avant de rendre une playlist
`file://` : aucune lecture avant la fin du transfert, contraire à la promesse
« clic, ça joue ». Trois options : (A) serveur HTTP local dans le noyau
servant une playlist VOD complète et récupérant les segments à la demande ;
(B) playlist fichier qui grandit (EVENT) ; (C) pousser les octets par FFI
dans chaque lecteur (AVAssetResourceLoader / appsrc / MediaStreamSource).

## Décision

**A.** Une session de lecture (`Node::open_stream`) sert
`http://127.0.0.1:<port>/<jeton>/index.m3u8` (VOD, `#EXT-X-ENDLIST`, durées
issues du manifeste → seek libre) ; un ordonnanceur donne la priorité aux
segments demandés par le lecteur puis à une fenêtre d'avance de 90 s ;
récupération via `get_with` (fetch multi-fournisseurs, modération #2, repli
froid inchangés). B est incompatible avec le seek libre et le rechargement
d'une playlist `file://` est incertain sur Media Foundation/GStreamer ; C
mettrait de la logique de lecture dans les trois fronts.

## Conséquences

- `fetch_hls` est retiré du FFI (contrat v11) et reste au CLI comme export
  hors ligne.
- Le cœur écoute sur `127.0.0.1` (jeton 128 bits, GET/HEAD, `Range`) ;
  aucune liste de sessions exposée. Un autre process local ne peut pas
  deviner l'URL, mais un process qui lit la mémoire ou les logs du front le
  peut — périmètre identique à un fichier temporaire lisible.
- Politique de stockage inchangée : `Seed` si channel souscrit (et réveil du
  seed proactif à la complétion → SeedIndex sous quota), `Stream` sinon
  (cache `<blocs>/.streams/<id>/` purgé à la fermeture et à l'ouverture du
  nœud).
