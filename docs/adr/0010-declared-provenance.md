# 0010 — Provenance déclarée, pas vérifiée

- Statut : accepté
- Date : 2026-09-06

## Contexte

Le projet se présentait comme une plateforme de « contenu généré par IA »
sans qu'aucun champ ne le porte : le feed v3 ne connaissait que cid, titre et
tags. Trois issues : abandonner le positionnement ; le rendre déclaratif ;
aller jusqu'à la preuve technique (C2PA / Content Credentials).

## Décision

**Déclaration obligatoire et signée** par entrée de feed (`champinium-feed/v4`) :
un mode (`generated`, `assisted`, `captured`, `undeclared` — valeur explicite,
jamais un défaut) et jusqu'à 8 outils en texte libre normalisé, cherchables
comme les tags (index local et DHT sous le même préfixe). Pas de prompt.
La déclaration vit dans le feed, pas dans le manifeste HLS : seul le feed est
signé par une clé, donc seule cette forme est attribuable — un manifeste peut
être listé par n'importe qui (même faille que la censure par injection,
ADR/lot d).

**C2PA est différé** : le réencodage HLS à l'ingestion casse la signature du
fichier source, et le bénéfice n'existe que si les outils des créateurs
signent. À rouvrir quand une part significative des sources arrivera avec des
Content Credentials, en conservant la déclaration comme repli.

## Conséquences

- Une déclaration est une **affirmation signée du publieur** : elle prouve
  qui affirme, pas ce qui est affirmé. Les fronts l'affichent sans la
  qualifier de « vérifiée ». La modération ne la lit pas.
- Zéro-compat : les feeds v3 sont rejetés. Contrat FFI v12, `publish_feed`
  sans métadonnées retiré.
- Conséquence du zéro-compat : un pair qui émet encore `champinium-feed/v3`
  voit ses feeds rejetés au parsing, ce que la validation applicative
  gossipsub rapporte comme `Reject` — son score de pair se dégrade jusqu'au
  graylistage. C'est un *flag day* assumé tant qu'il n'y a pas d'utilisateurs
  réels, pas seulement un feed ignoré.
- Positionnement reformulé : « contenu à provenance déclarée ».
