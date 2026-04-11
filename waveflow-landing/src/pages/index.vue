<template>
  <div class="selection:bg-emerald-500/30 selection:text-emerald-400">
    <HeroSection />
    <FeaturesSection />
    <AppPreview />
    <TechStack />
    <FooterSection />
  </div>
</template>

<script lang="ts" setup>
import { onMounted } from 'vue'
import HeroSection from '@/components/landing/HeroSection.vue'
import FeaturesSection from '@/components/landing/FeaturesSection.vue'
import AppPreview from '@/components/landing/AppPreview.vue'
import TechStack from '@/components/landing/TechStack.vue'
import FooterSection from '@/components/landing/FooterSection.vue'

onMounted(() => {
  const reveals = document.querySelectorAll('.reveal')

  const observer = new IntersectionObserver(
    (entries) => {
      entries.forEach((entry) => {
        if (entry.isIntersecting) {
          entry.target.classList.add('active')
        }
      })
    },
    { threshold: 0.15 },
  )

  reveals.forEach((el) => observer.observe(el))
})
</script>
