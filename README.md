# WaveFlow

**WaveFlow** est une application desktop de lecture musicale locale, construite avec Tauri 2. Elle offre une interface 3 panneaux inspirée des lecteurs modernes pour parcourir et écouter votre collection audio personnelle, avec support complet du mode clair/sombre et de plusieurs langues.

## Stack technique

### Frontend

- **React 19** — composants et gestion d'état
- **TypeScript** — typage strict
- **Vite** — dev server et bundler
- **Tailwind CSS 4** — système de design
- **i18next + react-i18next** — internationalisation (FR/EN)
- **Lucide React** — icônes

### Backend / Desktop

- **Tauri 2** — shell desktop multi-plateforme
- **Rust** — backend natif

### Outils

- **Bun** — gestionnaire de paquets et runtime
- **ESLint** + **Prettier** — lint et formatage

## Fonctionnalités actuelles (scaffolding UI)

- Interface 3 panneaux (sidebar, contenu principal, file de lecture)
- Mode clair / sombre avec transition radiale animée (View Transitions API)
- Internationalisation FR/EN avec détection automatique et persistance locale
- Accessibilité : navigation clavier, ARIA, `role="listbox"`, `role="switch"`, `role="slider"`
- Système de profils avec création (nom + couleur)
- Sélection de bibliothèque via popover
- Création de bibliothèques et playlists (nom, description, couleur, icône)
- Contrôles de lecture : play/pause, shuffle, repeat (off/all/one)
- Slider de volume interactif (pointer + clavier + mute)
- Empty states unifiés avec animation "breathing" du halo

## Commandes

```bash
# Installer les dépendances
bun install

# Lancer l'app desktop en mode dev (Vite + shell Tauri)
bun run tauri dev

# Builder l'app desktop en production
bun run tauri build

# Lancer uniquement le dev server Vite (sans shell Tauri)
bun run dev

# Vérifier les types
bun run typecheck

# Linter
bun run lint
bun run lint:fix

# Formater
bun run format
```

## Structure du projet

```bash
waveflow/
├── src/                          # Frontend React
│   ├── components/
│   │   ├── common/               # Composants réutilisables (StatCard, EmptyState, modals...)
│   │   ├── layout/               # Sidebar, TopBar, AppLayout, QueuePanel, DeviceMenu
│   │   ├── player/               # PlayerBar, PlaybackControls, VolumeControl, ProgressBar
│   │   └── views/                # HomeView, LibraryView, LikedView, RecentView, SettingsView, etc.
│   ├── contexts/                 # ThemeContext, PlayerContext
│   ├── hooks/                    # useTheme, usePlayer
│   ├── i18n/
│   │   ├── index.ts              # Configuration i18next
│   │   └── locales/
│   │       ├── fr.json           # Traductions françaises
│   │       └── en.json           # Traductions anglaises
│   ├── types/                    # Types TypeScript partagés
│   ├── app.css                   # Tailwind + utilities custom (animations, scrollbar)
│   ├── App.tsx
│   └── main.tsx
├── src-tauri/                    # Backend Rust / Tauri
│   ├── src/
│   │   ├── main.rs
│   │   └── lib.rs                # Commands Tauri
│   ├── Cargo.toml
│   ├── tauri.conf.json           # Config fenêtre, bundle, permissions
│   └── capabilities/
├── public/                       # Assets statiques
└── package.json
```

## Internationalisation

Les strings sont externalisés dans [src/i18n/locales/](src/i18n/locales/). Pour ajouter une nouvelle clé :

1. Ajouter la clé dans `fr.json` et `en.json` au bon namespace (ex. `home.banner.title`)
2. Dans le composant : `const { t } = useTranslation();` puis `t("home.banner.title")`
3. Pour les pluriels : utiliser les suffixes `_zero`, `_one`, `_other` et appeler avec `t("key", { count: n })`

Pour ajouter une langue :

1. Créer `src/i18n/locales/xx.json` avec la même structure que `fr.json`
2. L'importer dans [src/i18n/index.ts](src/i18n/index.ts) et l'ajouter à `SUPPORTED_LANGUAGES`
3. Elle apparaîtra automatiquement dans le sélecteur de langue des paramètres

## Accessibilité

- Tous les boutons interactifs ont un `aria-label` explicite
- Focus rings visibles au clavier (`focus-visible:ring-*`)
- `prefers-reduced-motion` respecté (animations `breathing` désactivées)
- Structure sémantique avec `<section aria-labelledby>` et `<h1>/<h2>`
- Toggles : `role="switch"` + `aria-checked`
- Sliders : `role="slider"` + `aria-valuemin/max/now`
- Dropdowns : `role="listbox"` + `aria-activedescendant` + navigation flèches

## Architecture Tauri

Les commandes Tauri sont définies dans [src-tauri/src/lib.rs](src-tauri/src/lib.rs) avec `#[tauri::command]` et enregistrées dans `invoke_handler`. Côté frontend, on les appelle avec :

```ts
import { invoke } from "@tauri-apps/api/core";

const result = await invoke("command_name", { arg: value });
```

L'identifiant de l'application est défini dans [src-tauri/tauri.conf.json](src-tauri/tauri.conf.json) : `app.waveflow`.

## Licence

GPL-3.0 — voir [LICENSE](LICENSE)
