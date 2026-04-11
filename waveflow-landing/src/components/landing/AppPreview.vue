<template>
  <section class="py-24 px-6 overflow-hidden relative z-10">
    <div class="text-center max-w-3xl mx-auto mb-16 reveal">
      <h2 class="font-display font-bold text-4xl md:text-5xl mb-4 text-text-main">
        {{ $t('preview.title') }}
      </h2>
      <p class="text-text-secondary text-lg">
        {{ $t('preview.subtitle') }}
      </p>
    </div>

    <div class="perspective-container max-w-6xl mx-auto reveal">
      <div class="mockup-3d mockup-3d-rotate w-full aspect-[4/3] md:aspect-[16/10] bg-dark-surface1 rounded-2xl border border-white/10 glow-emerald flex flex-col overflow-hidden shadow-2xl">

        <!-- Zone supérieure (3 panneaux) -->
        <div class="flex flex-1 overflow-hidden">

          <!-- Sidebar -->
          <div class="hidden md:flex flex-col w-64 bg-dark-bg border-r border-white/5 p-4">
            <div class="flex items-center gap-2 mb-8 px-2">
              <div class="w-3 h-3 rounded-full bg-red-500" />
              <div class="w-3 h-3 rounded-full bg-yellow-500" />
              <div class="w-3 h-3 rounded-full bg-green-500" />
            </div>
            <div class="space-y-4 mb-8">
              <div class="flex items-center gap-3 text-text-secondary px-2 py-1 rounded">
                <Home class="w-5 h-5" /> Accueil
              </div>
              <div class="flex items-center gap-3 text-emerald-400 px-2 py-1 rounded bg-white/5">
                <Search class="w-5 h-5" /> Recherche
              </div>
              <div class="flex items-center gap-3 text-text-secondary px-2 py-1 rounded">
                <LibraryIcon class="w-5 h-5" /> Bibliothèque
              </div>
            </div>
            <div class="text-xs font-bold text-text-tertiary uppercase tracking-wider mb-3 px-2">Playlists</div>
            <div class="space-y-2 overflow-y-auto flex-1">
              <div class="text-text-secondary text-sm px-2 py-1">Synthwave Mix</div>
              <div class="text-text-secondary text-sm px-2 py-1">Chill Vibes</div>
              <div class="text-text-secondary text-sm px-2 py-1">Focus Session</div>
              <div class="text-text-secondary text-sm px-2 py-1">Electronic 2025</div>
            </div>
          </div>

          <!-- Contenu Principal -->
          <div class="flex-1 bg-gradient-to-b from-dark-surface2 to-dark-surface1 p-6 overflow-hidden flex flex-col">
            <div class="flex justify-between items-center mb-8">
              <div class="flex gap-2">
                <button class="w-8 h-8 rounded-full bg-dark-bg flex items-center justify-center text-text-secondary">
                  <ChevronLeft class="w-5 h-5" />
                </button>
                <button class="w-8 h-8 rounded-full bg-dark-bg flex items-center justify-center text-text-secondary">
                  <ChevronRight class="w-5 h-5" />
                </button>
              </div>
              <div class="w-8 h-8 rounded-full bg-dark-surface2 border border-white/10 flex items-center justify-center">
                <User class="w-4 h-4 text-text-secondary" />
              </div>
            </div>

            <h2 class="text-2xl font-bold mb-4 text-white">Récemment écouté</h2>
            <div class="grid grid-cols-2 lg:grid-cols-4 gap-4 mb-8">
              <div
                v-for="album in albums"
                :key="album.title"
                class="p-4 rounded-xl border border-white/5 group"
                :class="album.hideOnMobile ? 'hidden lg:block' : ''"
                style="background: rgba(24, 24, 24, 0.5)"
              >
                <div class="aspect-square bg-dark-bg rounded-lg mb-3 relative overflow-hidden shadow-lg">
                  <div class="absolute inset-0" :class="album.gradient" />
                  <div class="absolute bottom-2 right-2 w-10 h-10 bg-emerald-500 rounded-full flex items-center justify-center shadow-lg opacity-0 group-hover:opacity-100 transition-opacity">
                    <Play class="w-5 h-5 text-dark-bg fill-current ml-1" />
                  </div>
                </div>
                <div class="font-semibold text-white truncate">{{ album.title }}</div>
                <div class="text-sm text-text-secondary truncate">{{ album.artist }}</div>
              </div>
            </div>

            <!-- Tracklist -->
            <div class="flex-1 overflow-hidden">
              <div class="flex items-center text-text-tertiary text-xs border-b border-white/5 pb-2 mb-2 px-4">
                <div class="w-8">#</div>
                <div class="flex-1">TITRE</div>
                <div class="hidden lg:block flex-1">ALBUM</div>
                <div class="w-12">
                  <Clock class="w-4 h-4" />
                </div>
              </div>
              <div class="flex items-center text-sm px-4 py-2 rounded-lg" style="background: rgba(255,255,255,0.03)">
                <div class="w-8 text-emerald-400">
                  <BarChart2 class="w-4 h-4" />
                </div>
                <div class="flex-1">
                  <div class="text-emerald-400 font-medium">Resonance</div>
                  <div class="text-text-secondary text-xs">HOME</div>
                </div>
                <div class="hidden lg:block flex-1 text-text-secondary">Odyssey</div>
                <div class="w-12 text-text-secondary">3:32</div>
              </div>
              <div class="flex items-center text-sm px-4 py-2 rounded-lg">
                <div class="w-8 text-text-secondary">2</div>
                <div class="flex-1">
                  <div class="text-white font-medium">Tech Noir</div>
                  <div class="text-text-secondary text-xs">Gunship</div>
                </div>
                <div class="hidden lg:block flex-1 text-text-secondary">Gunship</div>
                <div class="w-12 text-text-secondary">4:57</div>
              </div>
            </div>
          </div>

          <!-- Queue -->
          <div class="hidden lg:flex flex-col w-72 bg-dark-bg border-l border-white/5 p-4">
            <h3 class="font-bold text-white mb-4 px-2">File d'attente</h3>
            <div class="text-xs font-bold text-text-tertiary uppercase tracking-wider mb-3 px-2">En cours</div>
            <div class="flex items-center gap-3 p-2 bg-white/5 rounded-lg border border-white/5 mb-4">
              <div class="w-10 h-10 bg-dark-surface2 rounded flex-shrink-0 relative overflow-hidden">
                <div class="absolute inset-0 bg-gradient-to-br from-emerald-500/40 to-teal-500/40" />
              </div>
              <div class="overflow-hidden">
                <div class="text-emerald-400 text-sm font-semibold truncate">Resonance</div>
                <div class="text-text-secondary text-xs truncate">HOME</div>
              </div>
            </div>
            <div class="text-xs font-bold text-text-tertiary uppercase tracking-wider mb-3 px-2">À suivre</div>
            <div class="space-y-1 overflow-y-auto">
              <div
                v-for="track in queueTracks"
                :key="track.title"
                class="flex items-center gap-3 p-2 rounded-lg"
              >
                <div class="w-8 h-8 bg-dark-surface2 rounded flex-shrink-0 relative overflow-hidden">
                  <div class="absolute inset-0" :class="track.gradient" />
                </div>
                <div class="overflow-hidden">
                  <div class="text-white text-sm truncate">{{ track.title }}</div>
                  <div class="text-text-secondary text-xs truncate">{{ track.artist }}</div>
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- Player bar -->
        <div class="h-24 bg-dark-surface2 border-t border-white/10 flex items-center px-4 md:px-6 justify-between">
          <div class="flex items-center gap-4 w-1/3 min-w-0">
            <div class="w-14 h-14 bg-dark-bg rounded-md shadow-md relative overflow-hidden flex-shrink-0">
              <div class="absolute inset-0 bg-gradient-to-br from-emerald-500/40 to-teal-500/40" />
            </div>
            <div class="truncate">
              <div class="text-white font-medium truncate">Resonance</div>
              <div class="text-text-secondary text-xs truncate">HOME</div>
            </div>
            <button class="text-emerald-400 ml-2 hidden sm:block">
              <Heart class="w-4 h-4 fill-current" />
            </button>
          </div>

          <div class="flex flex-col items-center justify-center w-1/3 max-w-md">
            <div class="flex items-center gap-4 md:gap-6 mb-2">
              <button class="text-text-secondary hidden sm:block"><Shuffle class="w-4 h-4" /></button>
              <button class="text-text-secondary"><SkipBack class="w-5 h-5 fill-current" /></button>
              <button class="w-8 h-8 md:w-10 md:h-10 bg-white rounded-full flex items-center justify-center shadow-[0_0_15px_rgba(255,255,255,0.3)]">
                <Pause class="w-4 h-4 md:w-5 md:h-5 text-dark-bg fill-current" />
              </button>
              <button class="text-text-secondary"><SkipForward class="w-5 h-5 fill-current" /></button>
              <button class="text-text-secondary hidden sm:block"><Repeat class="w-4 h-4" /></button>
            </div>
            <div class="w-full flex items-center gap-2 text-xs text-text-tertiary">
              <span>1:24</span>
              <div class="flex-1 h-1.5 bg-dark-bg rounded-full overflow-hidden flex">
                <div class="w-1/3 h-full bg-gradient-emerald relative">
                  <div class="absolute right-0 top-1/2 -translate-y-1/2 w-2.5 h-2.5 bg-white rounded-full shadow" />
                </div>
              </div>
              <span>3:32</span>
            </div>
          </div>

          <div class="hidden md:flex items-center justify-end gap-3 w-1/3 text-text-secondary">
            <button><Mic2 class="w-4 h-4" /></button>
            <button><ListVideo class="w-4 h-4" /></button>
            <button><MonitorSpeaker class="w-4 h-4" /></button>
            <div class="flex items-center gap-2 w-24">
              <Volume2 class="w-4 h-4" />
              <div class="flex-1 h-1.5 bg-dark-bg rounded-full overflow-hidden">
                <div class="w-2/3 h-full bg-white" />
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>

<script lang="ts" setup>
import {
  Home, Search, Library as LibraryIcon, ChevronLeft, ChevronRight, User,
  Play, Clock, BarChart2, Heart, Shuffle, SkipBack, Pause, SkipForward,
  Repeat, Mic2, ListVideo, MonitorSpeaker, Volume2,
} from 'lucide-vue-next'

const albums = [
  { title: 'Midnight City', artist: 'Synthwave Artists', gradient: 'bg-gradient-to-br from-purple-500/20 to-blue-500/20', hideOnMobile: false },
  { title: 'Deep Focus', artist: 'Ambient', gradient: 'bg-gradient-to-br from-emerald-500/20 to-teal-500/20', hideOnMobile: false },
  { title: 'Lo-Fi Beats', artist: 'Chillhop', gradient: 'bg-gradient-to-br from-orange-500/20 to-red-500/20', hideOnMobile: true },
  { title: 'Night Drive', artist: 'Electronic', gradient: 'bg-gradient-to-br from-pink-500/20 to-rose-500/20', hideOnMobile: true },
]

const queueTracks = [
  { title: 'Tech Noir', artist: 'Gunship', gradient: 'bg-gradient-to-br from-purple-500/20 to-blue-500/20' },
  { title: 'Kavinsky', artist: 'Nightcall', gradient: 'bg-gradient-to-br from-pink-500/20 to-red-500/20' },
]
</script>
