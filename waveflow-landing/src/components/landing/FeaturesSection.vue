<template>
  <section class="py-24 px-6 max-w-7xl mx-auto z-10 relative">
    <div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-6">
      <div
        v-for="(feature, i) in features"
        :key="feature.key"
        class="glass-card p-8 reveal"
        :style="{ transitionDelay: `${(i + 1) * 0.1}s` }"
      >
        <div class="w-12 h-12 rounded-full bg-emerald-500/10 flex items-center justify-center mb-6 text-emerald-400">
          <component :is="feature.icon" class="w-6 h-6" />
        </div>
        <h3 class="text-xl font-bold text-text-main mb-3">
          {{ $t(`features.${feature.key}.title`) }}
        </h3>
        <p class="text-text-secondary text-sm leading-relaxed">
          {{ $t(`features.${feature.key}.desc`) }}
        </p>
      </div>
    </div>
  </section>
</template>

<script lang="ts" setup>
import { FolderOpen, Moon, Library, ListMusic } from 'lucide-vue-next'

const features = [
  { key: 'localPlayback', icon: FolderOpen },
  { key: 'themes', icon: Moon },
  { key: 'library', icon: Library },
  { key: 'queue', icon: ListMusic },
]
</script>
