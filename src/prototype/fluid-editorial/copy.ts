/**
 * Prototype copy, French + English only.
 *
 * The production app ships 17 locales through i18next; a design
 * prototype that may be thrown away should not add keys to 17 files.
 * The strings below are the ones a real integration would move into
 * `src/i18n/locales/*.json` — the existing keys they mirror are named
 * in the comments.
 */

export type Lang = "fr" | "en";

export function pickLang(language: string | undefined): Lang {
  return language?.toLowerCase().startsWith("fr") ? "fr" : "en";
}

const FR = {
  // home.greeting.*
  greeting: {
    morning: "Bonjour",
    afternoon: "Bonjour",
    evening: "Bonsoir",
    night: "Bonne nuit",
  },
  canvas: {
    resumeKicker: "Reprendre là où vous étiez",
    resume: "Reprendre",
    pause: "Pause",
    playAlbum: "Lire l’album",
    upNext: "Ensuite",
    recentStrip: "Dernières écoutes",
    remaining: "{{time}} restantes",
    radio: "Radio",
  },
  // home.stats.* folded into one line
  stats: {
    tracks: "titres",
    albums: "albums",
    liked: "coups de cœur",
    listened: "d’écoute",
  },
  // home.moodRadio.*
  mood: {
    title: "Radio par humeur",
    subtitle:
      "Un spectre de tempo : choisissez une humeur, WaveFlow compose la radio.",
    start: "Lancer la radio",
    starting: "Démarrage…",
    tracks: "{{count}} titres",
    empty: "Pas assez de titres analysés",
    bpm: "BPM",
    tempoAxis: "Tempo",
    moods: {
      sleep: {
        title: "Sommeil",
        subtitle: "Très lent et discret pour s’endormir",
      },
      focus: {
        title: "Concentration",
        subtitle: "Tempos calmes pour rester concentré",
      },
      chill: { title: "Détente", subtitle: "Ambiance posée, fins de journée" },
      party: { title: "Soirée", subtitle: "Tempo dansant, énergie haute" },
      workout: {
        title: "Entraînement",
        subtitle: "Cadence soutenue pour bouger",
      },
    },
  },
  // home.dailyMix.* + home.onRepeat.* …
  forYou: {
    title: "Pour vous",
    featuredKicker: "Vous y revenez sans cesse",
    featuredCta: "Réécouter",
    plays: "{{count}} écoutes",
    mixes: "Vos mixes",
    regenerate: "Régénérer",
    labels: {
      daily_mix: "Daily Mix",
      on_repeat: "On Repeat",
      artist_mix: "Mix d’artiste",
      genre_mix: "Mix de genre",
      decade_mix: "Mix de décennie",
    },
  },
  // home.recentlyAdded.* / home.recentlyPlayed.* / home.seeAll
  recentlyAdded: { title: "Récemment ajoutés", leadKicker: "Dernier ajout" },
  history: { title: "Récemment joués", listened: "écouté" },
  seeAll: "Tout voir",
  // sidebar.*
  sidebar: {
    home: "Accueil",
    webRadio: "Web Radio",
    myMusic: "Ma musique",
    tracks: "Titres",
    albums: "Albums",
    artists: "Artistes",
    genres: "Genres",
    years: "Années",
    folders: "Dossiers",
    playlists: "Playlists",
    liked: "Coups de cœur",
    recent: "Écoutés récemment",
    trackCount: "{{count}} titres",
    newPlaylist: "Nouvelle playlist",
  },
  // topbar.*
  topbar: {
    search: "Rechercher des titres, albums, artistes…",
    back: "Précédent",
    forward: "Suivant",
    sidebar: "Réduire la barre latérale",
    theme: "Thème clair",
    profile: "Profil",
  },
  // player.* / playerBar.*
  player: {
    noTrack: "Aucun titre",
    play: "Lire",
    pause: "Pause",
    previous: "Titre précédent",
    next: "Titre suivant",
    shuffle: "Lecture aléatoire",
    repeat: "Répéter",
    like: "Ajouter aux coups de cœur",
    lyrics: "Paroles",
    queue: "File d’attente",
    devices: "Périphériques de sortie",
    volume: "Volume",
    more: "Plus d’actions",
    immersive: "Vue immersive",
    miniPlayer: "Mini-lecteur",
    seek: "Position",
    bitPerfect: "Bit-perfect",
  },
  proto: {
    badge: "Prototype · Fluid Editorial",
    hint: "Navigation désactivée dans le prototype",
  },
};

type Copy = typeof FR;

const EN: Copy = {
  greeting: {
    morning: "Good morning",
    afternoon: "Good afternoon",
    evening: "Good evening",
    night: "Good night",
  },
  canvas: {
    resumeKicker: "Pick up where you left off",
    resume: "Resume",
    pause: "Pause",
    playAlbum: "Play the album",
    upNext: "Up next",
    recentStrip: "Last played",
    remaining: "{{time}} left",
    radio: "Radio",
  },
  stats: {
    tracks: "tracks",
    albums: "albums",
    liked: "liked",
    listened: "listened",
  },
  mood: {
    title: "Mood radio",
    subtitle: "A tempo spectrum: pick a mood, WaveFlow composes the radio.",
    start: "Start the radio",
    starting: "Starting…",
    tracks: "{{count}} tracks",
    empty: "Not enough analysed tracks",
    bpm: "BPM",
    tempoAxis: "Tempo",
    moods: {
      sleep: {
        title: "Sleep",
        subtitle: "Very slow and quiet for falling asleep",
      },
      focus: { title: "Focus", subtitle: "Calm tempos for deep work" },
      chill: { title: "Chill", subtitle: "Easy listening for winding down" },
      party: { title: "Party", subtitle: "Danceable tempos, high energy" },
      workout: { title: "Workout", subtitle: "Steady pace to keep moving" },
    },
  },
  forYou: {
    title: "For you",
    featuredKicker: "You keep coming back to it",
    featuredCta: "Play again",
    plays: "{{count}} plays",
    mixes: "Your mixes",
    regenerate: "Regenerate",
    labels: {
      daily_mix: "Daily Mix",
      on_repeat: "On Repeat",
      artist_mix: "Artist mix",
      genre_mix: "Genre mix",
      decade_mix: "Decade mix",
    },
  },
  recentlyAdded: { title: "Recently added", leadKicker: "Latest addition" },
  history: { title: "Recently played", listened: "played" },
  seeAll: "See all",
  sidebar: {
    home: "Home",
    webRadio: "Web Radio",
    myMusic: "My music",
    tracks: "Tracks",
    albums: "Albums",
    artists: "Artists",
    genres: "Genres",
    years: "Years",
    folders: "Folders",
    playlists: "Playlists",
    liked: "Liked tracks",
    recent: "Recently played",
    trackCount: "{{count}} tracks",
    newPlaylist: "New playlist",
  },
  topbar: {
    search: "Search tracks, albums, artists…",
    back: "Back",
    forward: "Forward",
    sidebar: "Collapse the sidebar",
    theme: "Light theme",
    profile: "Profile",
  },
  player: {
    noTrack: "No track",
    play: "Play",
    pause: "Pause",
    previous: "Previous track",
    next: "Next track",
    shuffle: "Shuffle",
    repeat: "Repeat",
    like: "Add to liked tracks",
    lyrics: "Lyrics",
    queue: "Queue",
    devices: "Output devices",
    volume: "Volume",
    more: "More actions",
    immersive: "Immersive view",
    miniPlayer: "Mini player",
    seek: "Position",
    bitPerfect: "Bit-perfect",
  },
  proto: {
    badge: "Prototype · Fluid Editorial",
    hint: "Navigation is disabled in the prototype",
  },
};

export function copyFor(lang: Lang): Copy {
  return lang === "fr" ? FR : EN;
}

/** `{{count}}`-style fill, the shape i18next uses, so moving a string
 *  into the locale files later is a cut and paste. */
export function fill(
  template: string,
  vars: Record<string, string | number>,
): string {
  return template.replace(/\{\{(\w+)\}\}/g, (_, k: string) =>
    String(vars[k] ?? ""),
  );
}
