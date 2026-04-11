import type { DisplayThresholds } from 'vuetify'

// repeated in settings.scss
const breakpoints: DisplayThresholds = {
  xs: 0,
  sm: 600,
  md: 960,
  lg: 1280,
  xl: 1920,
  xxl: 2560,
}

export const forVuetify = breakpoints

export const forUnoCSS = Object.entries(breakpoints)
  .reduce(
    (o, [key, value]) => ({ ...o, [key]: `${value}px` }),
    {} as Record<keyof DisplayThresholds, string>,
  )
