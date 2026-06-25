# 0004 — Conventional Commits (strict)

- Statut : accepté
- Date : 2026-06-25

## Contexte

Le versionnage et le CHANGELOG sont automatisés (voir
[ADR-0005](0005-release-please.md)), ce qui exige des messages de commit
structurés et fiables.

## Décision

Tous les commits suivent **[Conventional Commits](https://www.conventionalcommits.org/)**
(`type(scope): sujet`). Types usuels : `feat`, `fix`, `docs`, `chore`, `refactor`,
`test`, `ci`, `build`. Le projet utilise en plus des préfixes de périmètre d'agent
(`core:`, `macos:`, `win:`, `linux:`, `infra:`) compatibles avec ce format.
La CI valide les commits d'une PR via `cz check --rev-range origin/<base>..HEAD`.

## Conséquences

- release-please peut déduire les bumps de version et générer le CHANGELOG.
- Les commits de fusion doivent aussi être conventionnels (`chore: merge …`).
- L'historique antérieur à l'adoption n'est pas réécrit ; la règle vaut à partir
  de maintenant.
