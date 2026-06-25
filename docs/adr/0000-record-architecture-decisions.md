# 0000 — Enregistrer les décisions d'architecture

- Statut : accepté
- Date : 2026-06-25

## Contexte

Champinium prend de nombreuses décisions structurantes (transport P2P, modération,
feeds, FFI…). Sans trace, le « pourquoi » se perd et les choix sont rediscutés.

## Décision

Chaque décision d'architecture non triviale est consignée comme un ADR dans
`docs/adr/NNNN-<slug>.md`, format : **Statut · Contexte · Décision · Conséquences**.
Les ADR sont immuables une fois acceptés ; une décision révisée donne un nouvel
ADR qui supersède l'ancien.

## Conséquences

- Historique clair des choix et de leurs motivations.
- Les règles de gouvernance (audit intendant) peuvent référencer des ADR.
