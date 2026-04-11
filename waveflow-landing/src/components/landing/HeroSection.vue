<template>
  <!-- Aurora Background -->
  <div class="absolute inset-0 overflow-hidden pointer-events-none z-[-1]">
    <div class="absolute top-[10%] left-[20%] w-[500px] h-[500px] bg-emerald-500 rounded-full mix-blend-screen filter blur-[120px] opacity-20 animate-blob" />
    <div class="absolute top-[20%] right-[10%] w-[400px] h-[400px] bg-emerald-400 rounded-full mix-blend-screen filter blur-[120px] opacity-15 animate-blob delay-2000" />
    <div class="absolute bottom-[-10%] left-[40%] w-[600px] h-[600px] bg-emerald-600 rounded-full mix-blend-screen filter blur-[150px] opacity-20 animate-blob delay-4000" />
  </div>

  <section class="relative min-h-screen flex flex-col items-center justify-center px-6 pt-20 pb-16 text-center z-10">
    <!-- Logo animé -->
    <div class="mb-8 reveal">
      <WaveFlowLogo :size="80" animate />
    </div>

    <h1
      class="font-display font-extrabold text-6xl md:text-8xl tracking-tight mb-4 reveal"
      style="transition-delay: 0.1s"
    >
      <span class="text-gradient">WaveFlow</span>
    </h1>

    <h2
      class="text-2xl md:text-3xl font-medium text-text-main mb-6 reveal"
      style="transition-delay: 0.2s"
    >
      {{ $t('hero.tagline') }}
    </h2>

    <p
      class="text-text-secondary text-lg md:text-xl max-w-2xl mx-auto mb-10 reveal"
      style="transition-delay: 0.3s"
    >
      {{ $t('hero.subtitle') }}
    </p>

    <div class="flex flex-col sm:flex-row items-center gap-4 reveal" style="transition-delay: 0.4s">
      <button class="w-full sm:w-auto px-8 py-4 bg-gradient-emerald text-white rounded-full font-semibold text-lg shadow-[0_0_30px_rgba(16,185,129,0.3)] hover:shadow-[0_0_40px_rgba(16,185,129,0.5)] hover:scale-105 transition-all duration-300 flex items-center justify-center gap-2">
        <Download class="w-5 h-5" />
        {{ $t('hero.cta') }}
      </button>
      <a
        href="https://github.com/InstaZDLL/WaveFlow"
        target="_blank"
        rel="noopener"
        class="w-full sm:w-auto px-8 py-4 bg-transparent border border-white/20 text-text-main rounded-full font-semibold text-lg hover:bg-white/5 hover:border-white/40 transition-all duration-300 flex items-center justify-center gap-2 no-underline"
      >
        <Github class="w-5 h-5" />
        {{ $t('hero.ctaSecondary') }}
      </a>
    </div>
  </section>
</template>

<script lang="ts" setup>
import { Download, Github } from 'lucide-vue-next'
import WaveFlowLogo from './WaveFlowLogo.vue'
</script>
