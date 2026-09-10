# Passation — 2026-09-09

Document de reprise roulant. Il décrit l'état du chantier au moment où il a
été écrit, pas le produit : la documentation de produit vit dans
[`docs/`](docs/README.md) et reste la source de vérité. Ce fichier est
remplacé à chaque passation.

---

## 1. Où en est le travail

`main` = `cb3b7476`, CI verte. **Zéro alerte de sécurité ouverte** (voir 2.1).

### Issues ouvertes — 22, toutes en `planned` sauf #582

**Aucune n'est commencée.** « planned » veut dire triée, pas entamée.

Issues du triage des discussions (2026-09-07/08) :

| Issue | Sujet |
| --- | --- |
| **#578** | arbre de dossiers dans la bibliothèque |
| **#579** | recherche chinoise — **moitié sous-chaîne livrée**, reste le pinyin |
| **#581** | lecture Opus |
| **#582** | fenêtre de paroles flottante — `status: stalled` |
| **#583** | boutons de lecture sur la vignette de barre des tâches Windows |
| **#584** | paroles Apple Music mot à mot — **bloquée par #585** |
| **#585** | un monde de plugin capable de porter des paroles au mot |

Issues nées de l'analyse audio (2026-09-08) :

| Issue | Sujet |
| --- | --- |
| **#587** | mode album de ReplayGain — **héritera de `peak_unverified`, voir plus bas** |

Issues du rescan de l'audit croisé (2026-09-09, voir §3) :

| Issue | Sujet |
| --- | --- |
| **#588** | colonnes au choix, réordonnables et redimensionnables |
| **#589** | inventaire « à corriger » de la bibliothèque |
| **#590** | écriture des tags en place — **préalable du travail par lot** |
| **#591** | plus de champs de règle + compteur vivant (playlists intelligentes) |
| **#592** | écriture des tags dans les fichiers DSD |
| **#593** | capacités réelles de chaque périphérique de sortie |
| **#594** | filtrage des alias ALSA virtuels |
| **#595** | repli en rendu logiciel après un plantage GPU au démarrage |
| **#596** | mode contraste élevé |

Issues nées de la relecture de ce rapport (2026-09-09, voir §3) :

| Issue | Sujet |
| --- | --- |
| **#597** | rendre les dégradations de lecture visibles au lieu de les journaliser |
| **#598** | écritures fichier qui survivent à l'interruption, aux droits et à l'antivirus |
| **#599** | récupération de tags en ligne avec écran de revue — dépend de #598 |
| **#600** | sortie exclusive rouverte au taux source de chaque piste |
| **#601** | barre de tâches longues, avec annulation |

Plus `waveflow-android#32` (inclusion F-Droid) et **trois issues serveur** —
`waveflow-server#177` (le `PATCH` wholesale, seule à perdre des données), `#178`
(watermark) et `#179` (`/api/v2/songs`).

**Le triage des discussions est complet** : #557 → #578/#579, #519 → #581/#583
(son 3ᵉ point, la détection du `.lrc` homonyme, était déjà livré), #503 → #582,
#572 en `status: stalled` en attendant que son auteur teste ses touches
multimédia, #488 et #344 en `implemented`. Les trois auteurs ont eu une
réponse.

### PR ouvertes

| PR | Sujet | État |
| --- | --- | --- |
| **#486** | `chore(main): release 1.8.0` (release-please) | ouverte depuis la 1.7.0. **Ne jamais couper sans demande explicite.** Redemandé le 2026-09-07 : réponse « pas maintenant ». |

### Livré — trois issues entamées, deux closes

Les autres ne le sont pas. #579 est la seule ouverte à être à moitié faite.

**#579 (moitié sous-chaîne) — trouver une piste par le milieu de son titre**
(PR #605, 2026-09-10). `track_fts` passe en `trigram` et **possède désormais son
contenu**. Les deux étaient contraints, pas choisis :

- `unicode61` ne segmente pas le CJK — une suite ininterrompue de sinogrammes
  était **un seul token**, donc `人民` ne trouvait rien dans `中国人民解放军`
  alors que `中国` trouvait. Silencieusement, ce qui est la pire forme d'échec
  pour une recherche. Le défaut était en plus **asymétrique** :
  `search_albums` / `search_artists` utilisent `instr()`, donc la même requête
  trouvait l'album et pas la piste.
- Une table contentless ne rend pas ses colonnes : elles se relisent `NULL`,
  donc `LIKE` — la voie qui sert les termes sous le plancher de trois
  caractères de trigram — y renvoie **zéro**. D'où la table à contenu.
- `MATCH` ne trouve **rien** sous trois caractères, pas « moins bien ». C'est la
  forme courante d'un mot chinois, et `U2` / `M83` auraient régressé.

Mesures sur 50 000 pistes, qui ont tranché la question laissée ouverte par
l'issue : index 2,3 → 8,6 Mo, reconstruction **0,23 s** — donc dans la
migration, pas en backfill de fond. `MATCH` ~0,2 ms, scan ~26 ms.

**Une limite connue, volontairement non fermée** : `LIKE` ne replie la casse que
pour l'ASCII, donc un terme accentué de 1-2 caractères ne trouve plus sa forme
capitalisée (`ét` rate `Été indien`, `Ét` le trouve). Au-delà de 3 caractères
l'index replie casse **et** accents. La fermer demande une **colonne repliée sur
`track`** — la forme que `album` et `artist` ont déjà — soit une migration plus
une modification du scanner. Un test la fige, `library.md` la dit, et **le
pinyin a besoin de la même colonne**, donc les deux se feront probablement d'un
seul tenant.

**#580 — paroles dans le mini-lecteur** (PR #602, ouverte par jo-el414). Une
bascule `Mic2` dans la barre du haut ouvre les paroles dans le créneau que la
file d'attente occupait déjà. Trois décisions qui ne se lisent pas dans le
diff :

- **L'overlay n'est monté que quand il est ouvert.** Le mini-lecteur est une
  **seconde webview** : son instance de `useTrackLyrics` est une requête
  réellement distincte, pas un second consommateur de celle de la fenêtre
  principale. Le laisser monté derrière un overlay fermé aurait déclenché un
  `fetch_lyrics` de plus par piste, pour des paroles que personne ne regarde.
- **Un seul créneau d'état** (`"none" | "queue" | "lyrics"`) au lieu d'un
  booléen par panneau : les deux couvrent la même zone, deux drapeaux
  indépendants les laissaient s'empiler. Effet de bord : la zone
  pochette/titre/seek passe `inert` quel que soit l'overlay ouvert, alors que
  la file ne le faisait qu'en lecture locale.
- **Le mot actif prend le remplissage karaoké progressif**, via
  `useKaraokeWordFill`. Le panneau latéral garde volontairement la version
  discrète — c'est une bande à côté d'autre chose ; le mini-lecteur est une
  surface de lecture dédiée.

**#586 — un pic mesuré par l'analyse périmée n'est plus cru** (PR #603).
`track_analysis` porte désormais `analysis_version`, écrit par les deux chemins
de persistance depuis une constante posée à côté de `analyze_file`. Les lignes
antérieures lisent `NULL`, ce qui était exactement le signal manquant.
`TrackGain::peak_unverified` marque leur pic, et le limiteur le lit comme une
**borne inférieure** : plafond à 0 dB donc aucune amplification ne survit, mais
l'atténuation réclamée passe intégralement — un downmix qui dépasse déjà le
pleine échelle décrit un master qui écrête encore plus fort. Le balayage
reprend ces lignes pour qu'elles guérissent en étant re-mesurées ; il n'en
supprime toujours aucune, décision prise à #545 et non rouverte. La note de
`library.md` qui prétendait que l'écart résiduel était « borné par la
prévention d'écrêtage de toute façon » est corrigée : elle était fausse pour la
valeur dont cette prévention est **calculée**.

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

## 3. L'audit croisé — RESCAN FAIT, LE BLOCAGE EST LEVÉ

Un audit croisé d'un lecteur concurrent (nommé uniquement dans la mémoire de
l'agent — **consigne ferme de ne le citer nulle part** dans le code, les
commits, les PR ou la documentation) avait produit trois rangs d'items.

**Le rescan que le rang 2 attendait a eu lieu le 2026-09-09**, sur leur 0.2.3.
Il n'y a donc plus rien qui bloque : les reprises retenues sont devenues les
neuf issues **#588 à #596**, et le rang 2 comme le rang 3 n'existent plus comme
listes séparées.

**Rapport complet publié** :
https://claude.ai/code/artifact/80235e3e-11e3-436e-bbd6-d540a82d7e39

Trois choses à retenir de ce rescan, toutes détaillées dans la mémoire de
l'agent :

- **Leurs deux changelogs diffèrent.** Celui du dépôt couvre 14 versions contre
  7 sur le site, garde les *pourquoi*, et la date de la 0.1.9 diverge d'un mois.
  Lire les deux, et partir du code avant les deux.
- **Cinq « manques » n'en étaient pas** — demi-étoiles, downmix BS.775,
  sélection par plage, compteur d'écoutes, mesure ReplayGain. Vérifiés dans
  notre code. **Ne pas les rouvrir.**
- **Trois constats sont écartés volontairement** et n'ont pas d'issue : profils
  de qualité audio adaptés à la machine, décodage DSD multicanal parallèle,
  export sélectif et portable. Plus **l'atelier de tags complet**, non ouvert
  parce qu'il dépend de #590.

- **Rang 1 : clos.** PR #539 — 12 défauts dont trois pertes de données.
- **ReplayGain aux standards : clos.** PR #545 — BS.1770-4 complet. Son mode
  album, reporté à une 2ᵉ PR le 2026-08-24, est maintenant **#587** ; la
  fraîcheur des vieilles lignes d'analyse était **#586**, **livrée** (PR #603).
  #587 en hérite : le mode album doit plafonner avec le pic **d'album**, qui a
  exactement le même problème de fraîcheur dès qu'il est mesuré plutôt que lu
  dans un tag.

### Ce que les rangs 2 et 3 sont devenus — et ce qui reste sans issue

| Item d'origine | Devenu |
| --- | --- |
| Sentinelle GPU et bascule logicielle | **#595** |
| Filtrage des alias ALSA virtuels | **#594** |
| Capacités par périphérique, sondées à la demande | **#593** |
| Mode contraste élevé | **#596** |
| Atelier de tags | **#589** pour la moitié « inventaire » seulement |

**Les cinq derniers ont été ouverts le 2026-09-09** — ils étaient passés à
travers parce que le rapport de rescan était cadré en « qu'ont-ils que nous
n'avons pas », et que trois d'entre eux sont des constats sur **notre** code
plutôt que des fonctionnalités à reprendre :

| Item d'origine | Devenu |
| --- | --- |
| Rendre les dégradations visibles | **#597** |
| Sûreté d'écriture fichier | **#598** — à ne pas confondre avec #590 |
| Récupération de tags en ligne avec écran de revue | **#599** |
| Exclusif PCM piloté par le taux source | **#600** |
| Barre de tâches multiples avec annulation | **#601** |

**#590 traite la *vitesse* d'écriture, #598 sa *robustesse*.** Les deux sont des
préalables du travail par lot, pour des raisons différentes, et les confondre
ferait croire le second couvert par le premier.

**Il ne reste donc plus rien des rangs 2 et 3 sans issue**, à une exception
assumée : l'atelier de tags complet, dont seule la moitié « inventaire » est
ouverte (#589), le reste dépendant de #590 et #598.

## 4. Autres chantiers ouverts

### Demandes au serveur — désormais OUVERTES SUR SON DÉPÔT

Elles ne vivaient que dans ce fichier, donc l'agent serveur n'en avait aucune
trace. Ouvertes le 2026-09-08 : **waveflow-server#177** (le `PATCH`),
**#178** (le watermark), **#179** (`/api/v2/songs`). Le détail reste ici parce
que c'est le desktop qui en subit les conséquences.

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
3. `GET /api/v2/songs` exige un paramètre `genre`, donc ne sait pas lister les
   morceaux. Vérifié le 2026-09-08 : `("genre" = String, Query)` avec un `400
   "genre is required"`, handler nommé `list_songs_by_genre`, alors que
   `/songs/random` prend déjà `Option<String>`. Le vrai écart est entre le
   chemin et l'intention — soit la route devient `/by-genre`, soit le
   paramètre devient un filtre ; relâcher l'un sans renommer l'autre serait
   pire que les deux. N'affecte pas le desktop.

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
- **RFC-004 (base communautaire) : une question de provenance à trancher AVANT
  de brancher le premier contributeur.** La RFC est écrite depuis juin, statut
  Draft, et **rien n'est commencé** — zéro ligne de code des deux côtés,
  vérifié. Elle raisonne sur des contributions **saisies par un humain**, motif
  LRCLIB. Or le plan pour #584 est d'alimenter la base depuis un service **sous
  licence** : ce n'est pas la même chose, et récupérer pour soi n'est pas
  rediffuser à des gens qui n'ont pas d'abonnement. Le risque est concret parce
  que la donnée est **identifiable à la source** — minutages au mot, chanteur
  par ligne, chœurs marqués, clés de ligne : ça ne ressemble pas à des
  transcriptions communautaires. Piste qui sauve l'essentiel de la valeur :
  contribuer **l'alignement** et non le texte, le texte étant la partie
  protégée et le minutage un fait sur l'enregistrement — un client qui a déjà
  les paroles par LRCLIB ou par ses tags obtiendrait le karaoké au mot.
  Précédent interne : la RFC a déjà repoussé `cover_art` en v2 pour revue
  juridique, donc le réflexe existe, il faut juste l'appliquer à ce cas.
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
- **Mesurer avant d'annoncer une solution, pas seulement avant de la coder.**
  Pour #579 j'avais annoncé « trigram + `LIKE` » comme la clé ; la moitié était
  fausse pour ce schéma, parce qu'une table FTS5 **contentless ne rend pas ses
  colonnes** — elles se relisent `NULL` et `LIKE` y renvoie zéro. Trois autres
  faits du même genre, tous invisibles sans essai : `ESCAPE` **désactive**
  l'optimisation d'index du trigram (chercher le `:L0` dans
  `EXPLAIN QUERY PLAN`) ; `MATCH` ne renvoie **rien** sous trois caractères ;
  `LIKE` et `LOWER()` ne replient la casse que pour l'ASCII
  (`LOWER('ÉTÉ')` = `'ÉtÉ'`). Détail complet en mémoire d'agent.
- **Reconstruire une table FTS** : droper les triggers **d'abord**, puis la
  table (virtuelle et sans clé étrangère entrante, donc l'invariant « ne jamais
  droper une table parente » ne s'applique pas), recréer, repeupler. Vérifier
  avec `INSERT INTO t(t) VALUES('integrity-check')` **et** depuis une base déjà
  peuplée : c'est le seul test qui prouve le chemin de mise à jour.
- **Un marqueur de version testé contre `NULL` seul se désarme au premier
  bump.** Les deux sites de #586 comparaient `analysis_version IS NULL` :
  comportement identique en l'état, puisque `NULL` est la seule autre valeur
  qui existe — et identique pour toujours, ce qui était le défaut. Un passage
  de la constante à `2` aurait laissé les lignes en version 1 ni re-balayées ni
  méfiées, alors que le doc-commentaire de cette constante dit justement de la
  bumper. **Comparer à la constante**, et pas symétriquement : la **lecture**
  se méfie de toute génération inconnue (`!= Some(V)`, plus ancienne comme plus
  récente — refuser une amplification ne coûte rien quand on se trompe), la
  **ré-écriture** ne reprend que le strictement plus ancien (`IS NULL OR < V`)
  pour ne pas écraser les mesures d'un profil rapatrié d'un build plus récent.
  `NULL` garde toujours sa propre branche : `NULL < ?` vaut `NULL`, pas vrai.
  La vérification qui prouve le point : monter la constante d'un cran dans une
  requête jetée sur une vraie base, et voir l'ancienne génération redevenir
  éligible.
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
