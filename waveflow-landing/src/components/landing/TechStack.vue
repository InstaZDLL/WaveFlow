<template>
  <section class="py-16 border-y border-white/5 bg-dark-bg/50 backdrop-blur-sm relative z-10">
    <div class="max-w-4xl mx-auto px-6 text-center reveal">
      <h4 class="text-text-tertiary text-xs font-bold uppercase tracking-[0.2em] mb-8">
        {{ $t('techStack.title') }}
      </h4>
      <div class="flex flex-wrap justify-center items-center gap-10 md:gap-16">
        <div
          v-for="tech in techs"
          :key="tech.name"
          class="text-text-secondary opacity-50 hover:opacity-100 transition-all duration-300 font-display font-bold text-xl tracking-wider flex items-center gap-2"
          :class="tech.hoverColor"
        >
          <component :is="tech.icon" class="w-6 h-6" />
          {{ tech.name }}
        </div>
      </div>
    </div>
  </section>
</template>

<script lang="ts" setup>
import { Hexagon, Atom, FileCode2, Cog } from 'lucide-vue-next'

const techs = [
  { name: 'TAURI', icon: Hexagon, hoverColor: 'hover:text-white' },
  { name: 'REACT', icon: Atom, hoverColor: 'hover:text-[#61DAFB]' },
  { name: 'TYPESCRIPT', icon: FileCode2, hoverColor: 'hover:text-[#3178C6]' },
  { name: 'RUST', icon: Cog, hoverColor: 'hover:text-[#DEA584]' },
]
</script>
