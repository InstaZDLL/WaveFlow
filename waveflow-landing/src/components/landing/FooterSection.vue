<template>
  <footer class="border-t border-white/[0.06] py-10 relative z-10">
    <div class="max-w-7xl mx-auto px-6 flex flex-col md:flex-row items-center justify-between gap-6">
      <div class="flex items-center gap-3">
        <WaveFlowLogo :size="24" :animate="false" />
        <span class="font-display font-bold text-lg text-white tracking-wide">WaveFlow</span>
      </div>

      <div class="text-text-tertiary text-sm">
        {{ $t('footer.copyright') }}
      </div>

      <div class="flex items-center gap-6 text-sm text-text-secondary">
        <a
          href="https://github.com/InstaZDLL/WaveFlow"
          target="_blank"
          rel="noopener"
          class="hover:text-white transition-colors no-underline text-text-secondary"
        >
          {{ $t('footer.github') }}
        </a>
        <a
          href="https://github.com/InstaZDLL/WaveFlow/blob/main/LICENSE"
          target="_blank"
          rel="noopener"
          class="hover:text-white transition-colors no-underline text-text-secondary"
        >
          {{ $t('footer.license') }}
        </a>
      </div>
    </div>
  </footer>
</template>

<script lang="ts" setup>
import WaveFlowLogo from './WaveFlowLogo.vue'
</script>
