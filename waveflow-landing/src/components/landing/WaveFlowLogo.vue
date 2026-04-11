<template>
  <svg
    :width="size"
    :height="size"
    viewBox="0 0 64 64"
    class="mx-auto drop-shadow-[0_0_15px_rgba(16,185,129,0.5)]"
  >
    <defs>
      <linearGradient id="logo-gradient" x1="0%" y1="0%" x2="0%" y2="100%">
        <stop offset="0%" stop-color="#34D399" />
        <stop offset="50%" stop-color="#10B981" />
        <stop offset="100%" stop-color="#059669" />
      </linearGradient>
    </defs>
    <rect x="4" y="8" width="8" height="48" rx="4" fill="url(#logo-gradient)" :class="animate ? 'eq-bar eq-bar-1' : ''" />
    <rect x="16" y="16" width="8" height="32" rx="4" fill="url(#logo-gradient)" :class="animate ? 'eq-bar eq-bar-2' : ''" />
    <rect x="28" y="24" width="8" height="16" rx="4" fill="url(#logo-gradient)" :class="animate ? 'eq-bar eq-bar-3' : ''" />
    <rect x="40" y="16" width="8" height="32" rx="4" fill="url(#logo-gradient)" :class="animate ? 'eq-bar eq-bar-4' : ''" />
    <rect x="52" y="8" width="8" height="48" rx="4" fill="url(#logo-gradient)" :class="animate ? 'eq-bar eq-bar-5' : ''" />
  </svg>
</template>

<script lang="ts" setup>
withDefaults(defineProps<{
  size?: number
  animate?: boolean
}>(), {
  size: 80,
  animate: true,
})
</script>
