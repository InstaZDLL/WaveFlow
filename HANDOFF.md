# Passation — 2026-09-08

Document de reprise roulant. Il décrit l'état du chantier au moment où il a
été écrit, pas le produit : la documentation de produit vit dans
[`docs/`](docs/README.md) et reste la source de vérité. Ce fichier est
remplacé à chaque passation.

---

## 1. Où en est le travail

`main` = `bbc1773a`, CI verte. **Zéro alerte de sécurité ouverte** (voir 2.1).

### Issues ouvertes — toutes issues du triage des discussions

| Issue | Sujet | État |
| --- | --- | --- |
| **#578** | arbre de dossiers dans la bibliothèque | `planned` |
| **#579** | recherche chinoise par sous-chaîne + pinyin | `planned` |
| **#580** | paroles dans le mini-lecteur (ouverte par jo-el414) | `planned` |
| **#581** | lecture Opus | `planned` |
| **#582** | fenêtre de paroles flottante | `status: stalled` |
| **#583** | boutons de lecture sur la vignette de barre des tâches Windows | `planned` |

Plus `waveflow-android#32` (inclusion F-Droid) sur l'autre dépôt.

**Le triage des discussions est complet** : #557 → #578/#579, #519 → #581/#583
(son 3ᵉ point, la détection du `.lrc` homonyme, était déjà livré), #503 → #582,
#572 en `status: stalled` en attendant que son auteur teste ses touches
multimédia, #488 et #344 en `implemented`. Les trois auteurs ont eu une
réponse.

### PR ouvertes

| PR | Sujet | État |
| --- | --- | --- |
| **#486** | `chore(main): release 1.8.0` (release-please) | ouverte depuis la 1.7.0. **Ne jamais couper sans demande explicite.** Redemandé le 2026-09-07 : réponse « pas maintenant ». |

**#577 est mergée** (`fd61a631`, 2026-09-07) : sortie exclusive PCM sur Linux
et macOS. Elle existait pour le DoP uniquement — un fichier DSD pouvait
prendre le DAC en exclusif, un FLAC non. Validée sur matériel réel, son
entendu, sur les deux plateformes. Ce qu'elle ne prétend pas : le taux
d'échantillonnage reste une **préférence** côté PCM, c'est l'absence du
mixeur système et **pas** le taux source honoré de bout en bout. Le mot
« bit-perfect » a été retiré des libellés du réglage — la pastille du
pipeline peut encore l'afficher, mais elle vérifie en plus l'égalité des
taux. Faire suivre le taux source impose de rouvrir le périphérique à
chaque piste : c'est le chantier suivant, et c'est lui qui rendrait le mot
vrai.

## 2. Ce qui est en cours

### 2.1 Les deux alertes CodeQL sont classées — ne pas les rouvrir par réflexe

Les deux étaient `rust/non-https-url`, gravité haute. **Aucune ne se corrigeait
en forçant https**, et les deux sont classées avec leur justification dans
l'onglet sécurité. Si l'un de ces chemins change, l'alerte reviendra avec le
raisonnement qui l'avait fermée.

- **#23 — `core/src/artwork/motion_cache.rs:226` → faux positif.** `cache_mp4`
  rejette toute URL non-`https://` à son entrée (`is_safe_motion_url`, qui
  refuse aussi loopback / privé / lien-local) **avant** le moindre accès réseau
  ou disque, et chaque saut de redirection est revalidé par
  `redirect_decision`. Les deux comportements sont couverts par des tests.
  CodeQL ne relie pas la garde au point d'appel.
- **#24 — `app/src/audio/http_source.rs:268` → *won't fix*.** Ce chemin sert la
  Web Radio (les mounts Icecast / Shoutcast sont en clair dans leur immense
  majorité) **et** le serveur WaveFlow distant, couramment hébergé en HTTP sur
  un réseau local (`remote/playback.rs:294`). Le restreindre casserait les deux
  fonctionnalités. reqwest limite déjà les schémas à http/https.

À noter : les 22 alertes précédentes du dépôt sont toutes en `fixed`. Ces deux
là sont les premières écartées plutôt que corrigées.

### 2.2 Le cut 1.8.0

**Plus aucun bloqueur arbitré ne reste ouvert.** Les deux bloqueurs du
2026-08-30 sont tombés : les **underruns** étaient un câble HDMI défectueux
chez l'utilisateur, pas un défaut de WaveFlow ; l'**audio exclusif
ALSA / CoreAudio** est livré par #577. Cela ne veut pas dire « couper » :
#486 attend une décision explicite, et elle seule.

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

## 3. L'audit croisé — EN PAUSE

Un audit croisé d'un lecteur concurrent (nommé uniquement dans la mémoire de
l'agent — **consigne ferme de ne le citer nulle part** dans le code, les
commits, les PR ou la documentation) avait produit trois rangs d'items.

**Décision du 2026-09-07 : on met le rang 2 de côté**, parce qu'il faudra
d'abord repasser sur ce dépôt concurrent (il a bougé depuis la v0.2.1 sur
laquelle l'audit a été fait). **Ne rien lancer du rang 2 sans que ce
nouveau passage ait eu lieu.**

- **Rang 1 : clos.** PR #539 — 12 défauts dont trois pertes de données.
- **ReplayGain aux standards : clos.** PR #545 — BS.1770-4 complet.
- **Restent 4 items rang 2 et 4 items rang 3**, plus un cinquième chantier
  distinct :
  **le mode album de ReplayGain**, explicitement reporté à une 2ᵉ PR lors de
  l'arbitrage de #545 le 2026-08-24. Vérifié dans le code le 2026-09-07 :
  `rg_album_gain_db` / `rg_album_peak` sont lues des tags et stockées par le
  scanner, mais **rien ne les consomme à la lecture** — `TrackGain` ne porte
  qu'un couple gain/peak et `audio/` n'a aucune occurrence de `album_gain`.

### Rang 2 — bon rapport valeur / effort

1. **Rendre les dégradations visibles** — le backend réellement engagé
   affiché dans le lecteur (badge à 5 états), et un vrai retour d'erreur sur
   `player:error`, qui ne fait aujourd'hui qu'un `console.error`.
   *Recommandation posée, et #577 la renforce : il y a maintenant trois
   backends exclusifs capables de replier en silence.*
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
3. **Exclusif PCM piloté par le taux SOURCE** — **la moitié PCM est livrée
   par #577** ; reste la négociation au taux source, décrite en 1.
4. Mode contraste élevé ; barre d'état de tâches multiples avec annulation.

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
  y a eu des refus, jamais lesquels ni pourquoi. **Même famille** : les
  écritures de `profile_setting` sont best-effort dans 17 commandes de
  `commands/player.rs` (`if let Ok(pool)` + `let _ = sqlx::query`) contre 11
  qui propagent. Le partage est délibéré — best-effort là où le moteur a
  **déjà** changé, propagation là où rien n'a encore eu lieu — mais un échec
  d'écriture ne laisse aucune trace. À traiter en classe, sur les 17 sites,
  pas un à la fois.
- **Fin de RFC-006** : la génération par entité. Allégée par une découverte —
  la bijection complète absorbe déjà l'essentiel de l'ambiguïté, il ne reste
  que le cas étroit d'un ensemble entièrement examiné **puis** modifié.
- **Mutations en file orphelines d'une création refusée** — écarté en revue
  comme préexistant, à traiter séparément si ça se manifeste.
- Store de plugins phases 2 et 3 ; refonte des paroles traduites ; CI Gradle
  Android et inclusion F-Droid.

## 5. Pièges techniques appris récemment

### La machine de développement Linux est partagée

Elle a **11 Go de RAM et l'agent `waveflow-server` compile dessus en même
temps** (ses `ld` prennent ~1 Go pièce). Un `cargo test --workspace` sur le
crate `app` s'y fait tuer par le gestionnaire de mémoire, même en `-j 2`, et
les tâches de fond sont tuées avec lui — y compris les veilleurs de CI, ce
qui donne l'illusion d'un problème de CI. Réflexes : `free -g` avant toute
compilation Rust ; ne jamais tuer les processus de l'autre agent ; valider
par `cargo fmt --check` + `cargo clippy` (qui type-vérifie aussi les tests)
et laisser les **tests** du crate `app` au job CI `Rust (ubuntu-latest)`,
seul endroit où ils tournent de toute façon ; une vérification ponctuelle de
la CI plutôt qu'un poller.

### Audio

- **Ne jamais ouvrir un périphérique ALSA `hw:` sans demander une taille de
  période ET de tampon.** `HwParams::any` les laisse à ce que le pilote
  offre, et `snd_pcm_hw_params` prend alors son **maximum** pour les deux.
  Mesuré sur `snd-dummy` : période de 16 384 trames — soit ~370 ms à
  44,1 kHz — et un tampon assez profond pour que démarrer, chercher et
  changer de piste prennent une dizaine de secondes. **Cette attente était le
  tampon qui se vidait, pas la période.** La période a son propre effet, plus
  discret : elle est tirée de l'anneau en une seule passe et ce que l'anneau
  ne fournit pas est écrit en silence, ce qui rend l'underrun normal plutôt
  qu'exceptionnel.
- **ALSA et WASAPI alignent le 24 bits à l'envers l'un de l'autre.**
  `SND_PCM_FORMAT_S24_LE` place les 24 bits dans les trois octets **bas** du
  mot de 32 ; `WAVEFORMATEXTENSIBLE` dans les trois **hauts**. Recopier le
  décalage de l'autre backend multiplie chaque échantillon par 256 ; l'oubli
  dans l'autre sens atténue de 48 dB.
- **Le hog mode macOS s'enregistre contre un PID**, il n'éjecte donc pas un
  flux de notre propre processus. `must_release_before_reopening` répond
  « libérer d'abord » dans **deux** cas, pas un : un flux sortant exclusif sur
  n'importe quelle plateforme, **et** l'entrée en exclusif sous macOS depuis
  un flux **partagé**. Windows et Linux n'ont pas besoin de cet
  élargissement, ce qui explique que le cas macOS soit resté caché jusqu'à
  l'arrivée du PCM en hog mode.
- **Un underrun est aujourd'hui invisible** : anneau vide = `Err(_) => 0.0`
  dans le callback, sans compteur ni journal. Si le sujet revient,
  instrumenter **avant** de corriger.

### Méthode

- **Une référence de mesure se vérifie comme le reste.** Le codec Opus (#581)
  a été remesuré parce que quelqu'un a reproposé un décodeur pur-Rust. La
  mesure précédente comparait au décodage de `ffmpeg -f f32le` — donc au
  décodeur **natif** de ffmpeg, pas à libopus. Or le natif diverge lui-même de
  libopus de **2,6 dB** sur du SILK, tout en s'accordant à 79,5 dB sur du CELT.
  Une partie du verdict d'alors mesurait ffmpeg plutôt que le crate.
  **Toujours `-c:a libopus` avant `-i`.** Le protocole complet est dans le
  corps de #581, y compris le contrôle interne qui aurait dû alerter : si le
  CELT n'atteint pas le plafond de la référence, c'est le harnais qui est faux.
- **Vérifier chaque retour de revue contre le code, dans les deux sens.** Sur
  les 7 findings du robot sur #577, 5 étaient réels et 2 non — et
  inversement, une relecture de la PR avant sa sortie de brouillon a trouvé 4
  écarts que le robot n'a pas vus (trois commentaires macOS périmés, un
  commentaire de doc orphelin par insertion de fonction, un commentaire
  décrivant un reniflage d'UA que la PR supprimait).
- **La documentation ne suit pas toute seule.** #577 renommait un réglage,
  ajoutait deux backends et retirait « bit-perfect » des libellés sans
  toucher un seul fichier de doc. Vérifier `CLAUDE.md`, `docs/**` et le
  README à chaque changement.
- **Se méfier de ce document autant que du code.** Sa version précédente
  attribuait les dix secondes de démarrage ALSA à la période ; le
  commentaire source disait le contraire. Une passation recopiée n'est pas
  une vérification.
- **Exécuter les tests avant de les pousser, même quand la plateforme ne les
  exécute pas.** Les tests du crate `app` ne tournent pas sous Windows
  (`STATUS_ENTRYPOINT_NOT_FOUND`, DLL Tauri), et un module `cfg(linux)` ne
  s'y compile même pas. La parade : extraire les fonctions pures dans un
  **crate jetable du scratchpad**.
- **`sqlx::query` n'est pas vérifié à la compilation** (contrairement à
  `sqlx::query!`) : un nom de colonne faux survit à `cargo check` et à
  clippy.
- **Un symptôme rapporté n'est pas un défaut localisé.** Un des deux
  bloqueurs 1.8.0 s'est révélé être du matériel défectueux chez
  l'utilisateur. Demander système, périphérique, moment et format avant
  d'inscrire quoi que ce soit comme bloqueur.
- **APFS refuse un nom de fichier qui n'est pas de l'UTF-8 valide.** Un test
  `cfg(unix)` qui en crée un passe sous Linux et échoue sous macOS.
- **`cargo fmt` n'est pas optionnel** : première étape du job Rust, invisible
  pour `cargo check` comme pour clippy.

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
- **Répondre aux fils du robot de revue avec une mention `@coderabbitai`**,
  sinon il ne voit jamais la réponse et le fil reste ouvert.
- **Répondre aux contributeurs en quelques lignes chaleureuses, pas en dossier
  technique.** Quelqu'un qui relance en deux lignes amicales attend « c'est
  retenu, et voilà pourquoi ta proposition compte » — pas des noms de hooks ni
  des cadences d'événements. Le détail va dans le corps des issues qu'on écrit
  soi-même (#578 à #583 en sont l'exemple), où il sert à qui va coder. Ne vaut
  pas pour les fils de revue de code, où le détail est justement attendu.
- **Ne pas valoriser les lecteurs concurrents**, en particulier ceux à source
  ouverte, et **ne jamais nommer** celui de l'audit croisé.
- **Grouper les PR** plutôt que de les multiplier : le robot de revue est
  limité par compte, sur l'ensemble des dépôts.
- **17 locales, toutes complètes** : chaque clé ajoutée est propagée aux 17
  fichiers. `fr` est la source de vérité, le README est en anglais.
- **WaveFlow est GPL-3.0-only : ne jamais copier de code sous AGPL.**
