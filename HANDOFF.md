# Passation — 2026-09-06

Document de reprise roulant. Il décrit l'état du chantier au moment où il a
été écrit, pas le produit : la documentation de produit vit dans
[`docs/`](docs/README.md) et reste la source de vérité. Ce fichier est
remplacé à chaque passation.

---

## 1. Où en est le travail

`main` = `b550693b`, CI verte, **aucune issue ouverte** sur les quatre dépôts
sauf `waveflow-android#32` (inclusion F-Droid).

### PR ouvertes

| PR | Sujet | État |
| --- | --- | --- |
| **#486** | `chore(main): release 1.8.0` (release-please) | ouverte depuis la 1.7.0. **Ne jamais couper sans demande explicite.** |
| **#577** | Sortie exclusive PCM sur Linux et macOS | **brouillon**, validée sur matériel |

### PR #577 en deux lignes

La sortie exclusive existait sur Linux et macOS **pour le DoP uniquement** :
un fichier DSD pouvait prendre le DAC en exclusif, un FLAC non. Le
répartiteur le disait franchement — la branche `if exclusive` de
`spawn_output_with_mode` était `#[cfg(target_os = "windows")]`, et partout
ailleurs `let _ = exclusive;`. Les deux backends portent maintenant du PCM
ordinaire.

**Validé sur matériel réel, son entendu, sur les deux plateformes** :

- **Linux** — la carte est réclamée à PipeWire par le protocole de
  réservation, `S32_LE` négocié au taux du périphérique, latence correcte,
  bascule en cours de lecture propre.
- **macOS** — hog mode pris et rendu, AudioUnit au taux du périphérique, et
  le périphérique redevient disponible aux autres applications à la mort du
  processus (vérifié après un `SIGTERM`, donc sans passer par notre propre
  libération).

Au dernier point de contrôle : CI Windows et Frontend vertes, job Ubuntu
encore en cours. **Vérifier `gh pr checks 577` avant de conclure quoi que ce
soit** — le job Rust ubuntu est le seul qui exécute les tests du crate
`app`, et il est déjà resté rouge sans que personne le voie.

### Ce que #577 ne prétend pas

Le taux d'échantillonnage reste une **préférence** côté PCM : on prend ce que
le périphérique propose et le rééchantillonneur s'y adapte. C'est l'absence
du mixeur système, **pas** le taux source honoré de bout en bout. Le mot
« bit-perfect » a été retiré des libellés — il était déjà abusif sous
Windows, où le backend ouvre au format de l'endpoint et laisse rubato
convertir. Faire suivre le taux source impose de rouvrir le périphérique à
chaque piste : c'est la phase suivante, et c'est elle qui rendrait le mot
vrai.

---

## 2. La suite immédiate

### 2.1 Fermer #577

1. Sortir du brouillon (déclenche le robot de revue — répondre aux fils
   **avec une mention `@coderabbitai`**, sinon il ne voit jamais la réponse).
2. Merger **sur demande explicite uniquement**.

### 2.2 Le cut 1.8.0

Les bloqueurs avaient été arbitrés le 2026-08-30 : **underruns audio** et
**audio exclusif ALSA / CoreAudio**.

- Les **underruns sont écartés** (2026-09-06) : le symptôme venait d'un câble
  HDMI défectueux qui faisait bégayer l'affichage des paroles, pas l'anneau
  audio. Ce n'était pas un défaut de WaveFlow.
- L'**audio exclusif** est traité par #577.

Donc **plus aucun bloqueur arbitré ne reste ouvert** une fois #577 mergée.
Cela ne veut pas dire « couper » : #486 attend une décision explicite, et
elle seule.

Dettes réelles mais **non bloquantes**, à ne pas re-promouvoir sans
arbitrage : rotation et révocation des jetons de synchronisation v2 (jamais
validées bout en bout), et les notes de version DoP à écrire (opt-in par
défaut OFF, et « DoP vers un DAC non compatible = bruit blanc »).

### 2.3 Angle mort à connaître avant de toucher au code macOS

**Aucun job de CI ne construit ce projet sur macOS.** Tout ce qui est
`cfg(target_os = "macos")` — `coreaudio_exclusive.rs`, le DoP macOS, la
signature de code — n'est compilé ni en local sous Windows, ni en CI. Une
erreur de compilation y passerait un merge sans que rien ne la voie.

Une machine macOS est accessible et a servi à valider #577 : `cargo check`,
`cargo test` et `cargo clippy -D warnings` y tournent, et l'application peut
même être lancée en interface depuis une session SSH. **Les coordonnées
d'accès sont dans la mémoire de l'agent, délibérément pas ici** — ce dépôt
est public.

---

## 3. Le chantier suivant : les reprises de l'audit croisé

Un audit croisé d'un lecteur concurrent (nommé uniquement dans la mémoire de
l'agent — **consigne ferme de ne le citer nulle part** dans le code, les
commits, les PR ou la documentation) avait produit trois rangs d'items.

- **Rang 1 : clos.** PR #539 — 12 défauts dont trois pertes de données déjà
  livrées : le genre du fichier effacé à l'enregistrement des propriétés, les
  images intégrées détruites au changement de pochette, et les tags TXXX /
  Vorbis non standard perdus par le `Tag` générique de lofty.
- **ReplayGain aux standards : clos.** PR #545 — BS.1770-4 complet, mode
  album explicitement reporté à une seconde PR.
- **Restent 4 items rang 2 et 4 items rang 3.**

### Rang 2 — bon rapport valeur / effort

1. **Rendre les dégradations visibles** — le backend réellement engagé
   affiché dans le lecteur (badge à 5 états), et un vrai retour d'erreur sur
   `player:error`, qui ne fait aujourd'hui qu'un `console.error`.
   *Recommandation posée : suite logique de #539 et #545, tient en une PR,
   défaut invisible tant qu'on ne le cherche pas.*
2. **Sentinelle GPU et bascule logicielle** — filet générique pour ce que le
   correctif AppImage ne couvre pas. Peu coûteux, parce que le signal dont le
   mécanisme a besoin existe déjà (`app://ready`). À retenir : un plantage du
   processus WebKit doit **bloquer** le désarmement à la fermeture, sinon
   fermer la fenêtre blanche efface la trace.
3. **Sûreté d'écriture fichier** — `sync_all()` avant renommage, report des
   permissions, levée temporaire de l'attribut lecture seule sous Windows,
   réessais anti-antivirus. *Préalable au rang 3.*
4. **Filtrage des alias ALSA virtuels**, et capacités par périphérique
   **sondées à la demande** — pas à l'énumération : notre raccourci par les
   indices ALSA évite un gel de une à deux secondes et doit être préservé.

### Rang 3 — vrais chantiers

1. **Récupération de tags en ligne avec écran de revue.** Partir de leur
   module d'appariement (385 lignes, agnostique de la source) : trois indices
   pondérés — titre 0,60 / durée 0,25 / numéro 0,15 —, donnée absente = 0,5
   et non 0, affectation gloutonne globale où chaque piste distante est
   consommée une seule fois (ce qui empêche « Intro » de capturer un autre
   morceau), seuils 0,85 sûr et 0,55 douteux. **Porter sur notre
   `normalize_name`**, qui gère les marques combinantes NFD mieux que leur
   table latin-1. Prérequis : la sûreté d'écriture du rang 2.
2. **Atelier de tags** — tableur, audit « à corriger », règles de nettoyage,
   renommage par motif, journal annulable. Plusieurs PR.
3. **Exclusif PCM piloté par le taux SOURCE** — la phase suivante de #577,
   décrite en 1. C'est le sens audiophile de « bit-perfect ».
4. Mode contraste élevé ; barre d'état de tâches multiples avec annulation.

### Blocage à lever avant de démarrer un item du rang 2

L'utilisateur avait annoncé le 2026-08-24 vouloir **statuer sur autre chose
d'abord**, sans préciser quoi. Reposé le 2026-09-06, toujours sans réponse.
**Lui redemander avant de lancer quoi que ce soit du rang 2.**

---

## 4. Autres chantiers ouverts

### Demandes au serveur — le desktop est prêt, le serveur doit bouger

1. **Route « corrections brutes » de piste — la plus solide.** Le `PATCH` de
   piste est **wholesale** : le corps est l'ensemble complet des corrections,
   et un champ omis est une correction **retirée**. Or `GET /tracks/{id}`
   renvoie les valeurs **fusionnées** (fichier + surcharges), donc un
   lire-modifier-écrire figerait les valeurs du fichier en surcharges
   permanentes. Conséquence **reproduite, pas théorique** : un commentaire
   posé par un autre client est effacé par une correction desktop qui ne
   mentionne que le titre.
2. **Watermark du flux d'événements non exposé** — le client ne peut pas
   savoir à quel point son curseur est proche du refus.
3. `GET /api/v2/songs` exige un paramètre `genre` qui devrait être facultatif
   (n'affecte pas le desktop).

### Desktop

- **Refus silencieux hors bornes** : une année à 99999 passe la validation de
  la modale et meurt en 422 côté serveur. Le compteur des réglages dit qu'il
  y a eu des refus, jamais lesquels ni pourquoi.
- **Fin de RFC-006** : la génération par entité. Allégée par une découverte —
  la bijection complète absorbe déjà l'essentiel de l'ambiguïté, il ne reste
  que le cas étroit d'un ensemble entièrement examiné **puis** modifié.
- **Mutations en file orphelines d'une création refusée** — écarté en revue
  comme préexistant, à traiter séparément si ça se manifeste.
- Store de plugins phases 2 et 3 ; refonte des paroles traduites ; CI Gradle
  Android et inclusion F-Droid.

---

## 5. Pièges techniques appris récemment

### Audio

- **Ne jamais ouvrir un périphérique ALSA `hw:` sans demander une taille de
  période ET de tampon.** `HwParams::any` les laisse à ce que le pilote
  offre, et `snd_pcm_hw_params` prend alors son **maximum** : période de
  16 384 trames observée, et démarrage / recherche / changement de piste à
  dix secondes. Second effet, moins visible : **une période est tirée de
  l'anneau en une seule passe**, et ce que l'anneau ne fournit pas est écrit
  en silence — une période valant les deux tiers de l'anneau rend l'underrun
  normal plutôt qu'exceptionnel.
- **ALSA et WASAPI alignent le 24 bits à l'envers l'un de l'autre.**
  `SND_PCM_FORMAT_S24_LE` place les 24 bits dans les trois octets **bas** du
  mot de 32 ; `WAVEFORMATEXTENSIBLE` dans les trois **hauts**. Recopier le
  décalage de l'autre backend multiplie chaque échantillon par 256 ; l'oubli
  dans l'autre sens atténue de 48 dB. Les deux conventions coexistent dans le
  même moteur, à deux fichiers d'écart.
- **Le hog mode macOS s'enregistre contre un PID**, il n'éjecte donc pas un
  flux de notre propre processus. Le moteur ouvre le nouveau flux avant de
  fermer l'ancien — ce qui marche sous Windows (l'endpoint saisi éjecte le
  client partagé) et sous Linux (la réservation fait rendre la carte), mais
  produisait sous macOS un AudioUnit qui ne rend rien : pas de son, et le
  compteur de position gelé. La règle « libérer d'abord » couvre désormais ce
  cas.
- **Un underrun est aujourd'hui invisible** : anneau vide = `Err(_) => 0.0`
  dans le callback, sans compteur ni journal. Si le sujet revient,
  instrumenter **avant** de corriger, sinon on corrige à l'aveugle et on ne
  sait pas si ça a marché.

### Méthode

- **Exécuter les tests avant de les pousser, même quand la plateforme ne les
  exécute pas.** Les tests du crate `app` ne tournent pas sous Windows
  (`STATUS_ENTRYPOINT_NOT_FOUND`, DLL Tauri), et un module `cfg(linux)` ne s'y
  compile même pas. La parade qui a payé deux fois : extraire les fonctions
  pures dans un **crate jetable du scratchpad** et les exécuter là. Sans ça,
  trois tests avaient été poussés faux, chacun d'une manière différente.
- **`sqlx::query` n'est pas vérifié à la compilation** (contrairement à
  `sqlx::query!`) : un nom de colonne faux survit à `cargo check` et à
  clippy, et ne se manifeste qu'à l'exécution.
- **Un symptôme rapporté n'est pas un défaut localisé.** Un des deux
  bloqueurs 1.8.0 s'est révélé être du matériel défectueux chez
  l'utilisateur. Demander système, périphérique, moment et format avant
  d'inscrire quoi que ce soit comme bloqueur.
- **APFS refuse un nom de fichier qui n'est pas de l'UTF-8 valide.** Un test
  `cfg(unix)` qui en crée un passe sous Linux et échoue sous macOS.
- **`cargo fmt` n'est pas optionnel** : première étape du job Rust, invisible
  pour `cargo check` comme pour clippy, et un fichier non formaté fait
  échouer tout le job avant le moindre test.

---

## 6. Règles de travail à respecter

- **Ne jamais merger une PR ni couper une release sans demande explicite.**
  Vaut en particulier pour #486.
- **`E:\Workspace\WaveFlow` est une copie de travail partagée** avec un autre
  agent : ne jamais y changer de branche, passer par `git worktree`.
- **Répondre en français.**
- **Pas de tests unitaires frontend** dans ce dépôt : ne pas proposer de
  suite vitest ou jest, ne pas ouvrir d'issue de suivi pour ça.
- **Pas de backticks dans `git commit -m`** : le shell les exécute et avale
  le mot. Passer par `git commit -F` ou un heredoc cité.
- **Ne pas valoriser les lecteurs concurrents**, en particulier ceux à source
  ouverte, et **ne jamais nommer** celui de l'audit croisé.
- **Grouper les PR** plutôt que de les multiplier : le robot de revue est
  limité par compte, sur l'ensemble des dépôts.
- **17 locales, toutes complètes** : chaque clé ajoutée est propagée aux 17
  fichiers. `fr` est la source de vérité, le README est en anglais.
- **WaveFlow est GPL-3.0-only : ne jamais copier de code sous AGPL.**
