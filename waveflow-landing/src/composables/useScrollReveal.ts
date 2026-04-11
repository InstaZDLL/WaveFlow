import { ref, onMounted, onUnmounted, type Ref } from 'vue'

export function useScrollReveal(
  elementRef: Ref<HTMLElement | null>,
  options?: IntersectionObserverInit,
) {
  const isVisible = ref(false)
  let observer: IntersectionObserver | null = null

  onMounted(() => {
    if (!elementRef.value) return
    observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) {
          isVisible.value = true
          observer?.disconnect()
        }
      },
      { threshold: 0.15, ...options },
    )
    observer.observe(elementRef.value)
  })

  onUnmounted(() => observer?.disconnect())

  return { isVisible }
}
