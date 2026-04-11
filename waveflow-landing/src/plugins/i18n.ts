import { createI18n } from 'vue-i18n'

const messages = {
  fr: {
    hero: {
      tagline: 'Votre musique. Votre contrôle.',
      subtitle: 'Un lecteur de musique local, élégant et performant. Construit pour ceux qui possèdent leur bibliothèque musicale.',
      cta: 'Télécharger',
      ctaSecondary: 'Voir sur GitHub',
    },
    features: {
      title: 'Fonctionnalités',
      localPlayback: {
        title: 'Lecture locale',
        desc: 'Lisez vos fichiers audio directement depuis votre disque. Aucun streaming, aucune connexion requise.',
      },
      themes: {
        title: 'Thèmes sombre & clair',
        desc: 'Une interface qui s\'adapte à vos préférences avec un mode sombre immersif.',
      },
      library: {
        title: 'Gestion de bibliothèque',
        desc: 'Organisez, recherchez et parcourez votre collection musicale avec facilité.',
      },
      queue: {
        title: 'File d\'attente',
        desc: 'Gérez votre file d\'attente de lecture avec un panneau latéral intuitif.',
      },
    },
    preview: {
      title: 'Découvrez l\'interface',
      subtitle: 'Une expérience inspirée de Spotify, conçue pour vos fichiers locaux.',
    },
    techStack: {
      title: 'Construit avec',
    },
    footer: {
      copyright: '© 2025 WaveFlow. Tous droits réservés.',
      github: 'GitHub',
      license: 'Licence MIT',
    },
  },
  en: {
    hero: {
      tagline: 'Your music. Your control.',
      subtitle: 'A sleek, high-performance local music player. Built for those who own their music library.',
      cta: 'Download',
      ctaSecondary: 'View on GitHub',
    },
    features: {
      title: 'Features',
      localPlayback: {
        title: 'Local playback',
        desc: 'Play your audio files directly from your drive. No streaming, no connection required.',
      },
      themes: {
        title: 'Dark & light themes',
        desc: 'An interface that adapts to your preferences with an immersive dark mode.',
      },
      library: {
        title: 'Library management',
        desc: 'Organize, search and browse your music collection with ease.',
      },
      queue: {
        title: 'Queue system',
        desc: 'Manage your playback queue with an intuitive side panel.',
      },
    },
    preview: {
      title: 'Discover the interface',
      subtitle: 'A Spotify-inspired experience, designed for your local files.',
    },
    techStack: {
      title: 'Built with',
    },
    footer: {
      copyright: '© 2025 WaveFlow. All rights reserved.',
      github: 'GitHub',
      license: 'MIT License',
    },
  },
}

export default createI18n({
  legacy: false,
  locale: 'fr',
  fallbackLocale: 'en',
  messages,
})
