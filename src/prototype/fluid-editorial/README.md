# Fluid Editorial — prototype de la page d'accueil

Exploration visuelle, **pas une refonte approuvée**. Tout vit dans ce
dossier (plus un script, une entrée dans `main.tsx` et un middleware dev
dans `vite.config.ts`) : supprimer `src/prototype/`, `scripts/fluid-proto-fixtures.ts`,
`prototypes/` et les deux branches `?proto=` suffit à le retirer.

## Ouvrir le prototype

```bash
bun run scripts/fluid-proto-fixtures.ts   # une fois : pochettes + fixtures depuis votre profil
bun run dev                                # Vite seul, pas de coquille Tauri
```

Puis ouvrir **http://localhost:1420/?proto=fluid-editorial** dans un navigateur.

- Le script lit `%APPDATA%/app.waveflow/waveflow/profiles/2` par défaut
  (un autre dossier de profil se passe en argument). Il copie une soixantaine
  de pochettes redimensionnées dans `prototypes/fluid-editorial/covers/`
  (dossier **gitignoré**, servi uniquement par `vite dev` sous `/fluid-proto/`)
  et écrit `fixtures.generated.json` (titres, artistes, compteurs, palette
  par pochette — commité, sans image).
- Sans les pochettes, chaque tuile retombe sur un dégradé tiré de sa palette :
  la composition tient, en plus plat.
- La langue suit celle de l'app (`waveflow-language` dans localStorage) :
  français ou anglais, les 15 autres locales ne sont pas couvertes.
- Les captures de référence sont dans `prototypes/fluid-editorial/screenshots/`
  (1280×720, 1600×900, 1920×1080), régénérables avec Chrome headless.

## Ce qui change par rapport à l'accueil actuel

| Aujourd'hui                                         | Fluid Editorial                                                                                                                                                      |
| --------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Bannière de bienvenue + 4 cartes de statistiques    | **Listening Canvas** : salutation en italique éditoriale, les 4 compteurs sur une ligne de texte tertiaire, titre en cours en display 32–52 px, pochette 15–27 rem à droite avec son halo, les 5 derniers disques accrochés à son coin, la file « Ensuite » en 3 lignes denses. Aucune carte. |
| Fond uni                                            | **Atmosphère** : deux nappes radiales de la couleur moyenne de la pochette + grain. L'accent (bouton, barre active, lignes d'onde, progression) est la teinte vive de la pochette, désaturée et pincée en luminosité pour rester lisible. Retombe sur l'émeraude de la marque quand la pochette est grise. |
| Radio par humeur : 5 blocs saturés                  | **Spectre d'humeur** : une bande continue dont l'axe horizontal est le tempo (40→170 BPM). Chaque humeur est un segment avec sa propre onde (plus lente et plate à gauche, plus rapide et haute à droite). Sélection = le segment s'élargit et révèle fenêtre de tempo, nombre de titres, bouton « Lancer la radio ». Radiogroup clavier (←/→, Home/End, Entrée), ondes figées en `prefers-reduced-motion`. |
| « Pour vous » : bannières 16:7 identiques           | **Mosaïque éditoriale** : un album à la une (le plus écouté) en demi-largeur avec typographie magazine sur sa pochette, deux Daily Mix en portrait avec leur 2×2, les autres mixes en puces compactes. |
| « Récemment ajoutés » : grille uniforme             | **Étagère** : le disque le plus récent prend 4 fois la surface et une légende complète ; les autres restent de petits carrés, sans boîte autour. |
| « Récemment joués » : grille de 160 px              | **Journal** : liste sur deux colonnes (titre, artiste · album, il y a…, durée) — 12 lignes dans la hauteur où la grille en montrait 6. |
| Barre de lecture 96 px, bordure haute               | **84 px, sans bordure**, dégradé haut dans la couleur de la pochette. Trois groupes : identité (pochette 56 px, titre 15 px, artiste 12 px, étiquette mono `FLAC · 24/44.1`), transport autour d'un seul bouton plein, panneaux en deux grappes discrètes. **Progression en onde** : la partie lue est une sinusoïde de 2 px d'amplitude, le reste un filet. Aucune commande retirée. |
| Barre latérale : blocs remplis, libellés gras       | Même structure, chrome apaisé : pas de bordure, libellés 11 px médium espacés, état actif = barre d'accent de 2 px, compteurs à droite, **pochettes réelles** sur chaque playlist (2×2). |
| Barre supérieure vitrée avec bordure                | Même ton que le contenu, 52 px, glyphes nus ; seule la recherche garde sa pilule. |

### Système visuel

- **Typographie** : DM Sans Variable (déjà embarquée pour Liquid, axe optique) pour tout ;
  Playfair Display italique — déjà embarquée pour Editorial — réservée à la salutation et
  au chapeau de l'album à la une. Titres en 540–560, interlettrage −0,02 à −0,035 em.
- **Couleur** : fondation neutre `#0b0b0e` / `#121216` / `#17171d`, encre `#f3f1ec`,
  un seul accent, dérivé de la pochette. La bande d'humeur est la seule exception : le
  tempo est une dimension à part, sa teinte le dit.
- **Surfaces** : profondeur par ton et espacement, pas par bordures. Rayon 0,625–1,25 rem,
  pilules pour les actions.
- **Motif** : la courbe d'onde — en-têtes de section, progression du lecteur, segments d'humeur.
- **Mouvement** : montée en cascade à l'arrivée (opacité + 10 px, 640 ms), lever de 3 px au survol,
  élargissement du segment en 460 ms, dérive des ondes. Tout en `transform` / `opacity` sauf
  le `flex-grow` du segment (5 éléments, 460 ms). `prefers-reduced-motion` coupe tout.

## Ce qui est simulé

- **Lecture** : état local (`state.tsx`). « Reprendre » fait avancer une horloge ; cliquer un
  disque ou une ligne le met « en cours » et recolore la page. Rien ne parle au backend.
- **Radio par humeur** : « Lancer » choisit un titre dont le tempo tombe dans la fenêtre, au
  lieu d'appeler `start_mood_radio`. Les compteurs viennent d'une requête équivalente sur
  `track_analysis` au moment de la génération.
- **Navigation** : seule « Accueil » est active ; les autres entrées portent une infobulle.
- **Recherche, thème, profil, panneaux, volume, périphériques** : rendus, inertes.
- **Couleur de la pochette** : palette précalculée par `sharp` à la génération, raffinée à
  l'exécution par `dominantColor()` (la fonction de l'app) quand l'image est décodée.
- Dans un navigateur sans Tauri, un module de l'app journalise une erreur `invoke` au
  chargement (`useAccentChoice`) ; elle vient de l'arbre de production importé par `main.tsx`,
  pas du prototype.

## Pistes d'intégration

Le prototype n'emprunte aux composants de production que `WaveFlowLogo`, `PlaylistIcon`,
`resolvePlaylistColor` et `dominantColor`. Une intégration passerait par :

1. une skin ou un thème « Fluid » exposant `--fe-*` via `applySkin` ;
2. `HomeView` recomposé section par section (le canvas, la bande, la mosaïque, l'étagère, le journal) sur `PlayerContext` / `LibraryContext` ;
3. `ProgressBar` et `PlayerBar` recevant l'onde et le regroupement ;
4. les chaînes de `copy.ts` migrées dans les 17 locales.
