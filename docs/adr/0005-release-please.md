# 0005 — Versionnage automatisé avec release-please

- Statut : accepté
- Date : 2026-06-25

## Contexte

Maintenir manuellement versions, CHANGELOG et tags est source d'erreurs et
incohérent avec une cadence de petits commits atomiques.

## Décision

Utiliser **release-please** (`release-please-config.json`,
`.release-please-manifest.json`, workflow `.github/workflows/release-please.yml`).
Sur push `main`, l'action ouvre/maintient une « release PR » qui calcule la
prochaine version (depuis les Conventional Commits — [ADR-0004](0004-conventional-commits-strict.md)),
met à jour le CHANGELOG et, une fois mergée, crée le tag.

## Conséquences

- Versionnage et CHANGELOG déterministes, dérivés de l'historique.
- Dépend strictement de la conformité Conventional Commits.
