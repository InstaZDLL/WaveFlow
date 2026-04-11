import type { StaticRule } from 'unocss'
import { defineConfig, presetWind4, transformerDirectives } from 'unocss'
import { createThemeVariants, elevationPresets } from 'unocss-preset-vuetify'
import * as breakpoints from './src/theme/breakpoints'

export default defineConfig({
  presets: [
    presetWind4(),
  ],
  transformers: [
    transformerDirectives(),
  ],
  theme: {
    breakpoint: breakpoints.forUnoCSS,
    font: {
      heading: 'Syne, Roboto, sans-serif',
      body: 'Inter, Roboto, sans-serif',
      mono: '"Roboto Mono", sans-serif',
      display: '"Space Grotesk", Syne, sans-serif',
    },
    colors: {
      primary: 'rgb(var(--v-theme-primary))',
      dark: {
        bg: '#0A0A0A',
        surface1: '#121212',
        surface2: '#181818',
      },
      text: {
        main: '#E5E7EB',
        secondary: '#9CA3AF',
        tertiary: '#6B7280',
      },
    },
  },
  variants: createThemeVariants(['light', 'dark']),
  rules: [
    ...Object.entries(elevationPresets.md3)
      .map(([level, css]) => [`elevation-${level}`, css]) satisfies StaticRule[],
  ],
  safelist: [
    'font-heading', 'font-body', 'font-mono',
    ...Array.from({ length: 6 }, (_, i) => `elevation-${i}`),
    ...['', '-0', '-sm', '-lg', '-xl', '-pill', '-circle', '-shaped'].map(suffix => `rounded${suffix}`),
  ],
  outputToCssLayers: {
    cssLayerName: layer => layer === 'properties' ? null : `uno-${layer}`,
  },
  shortcuts: {
    'text-display-large': '  font-heading normal-case text-[3.5625rem] font-[400] leading-[1.1228] tracking-[-.0044em]',
    'text-display-medium': ' font-heading normal-case text-[2.8125rem] font-[400] leading-[1.1556] tracking-[normal]',
    'text-display-small': '  font-heading normal-case text-[2.25rem]   font-[400] leading-[1.2222] tracking-[normal]',
    'text-headline-large': ' font-heading normal-case text-[2rem]      font-[400] leading-[1.25]   tracking-[normal]',
    'text-headline-medium': 'font-heading normal-case text-[1.75rem]   font-[400] leading-[1.2857] tracking-[normal]',
    'text-headline-small': ' font-heading normal-case text-[1.5rem]    font-[400] leading-[1.3333] tracking-[normal]',
    'text-title-large': '    font-heading normal-case text-[1.375rem]  font-[400] leading-[1.2727] tracking-[normal]',
    'text-title-medium': '   font-body    normal-case text-[1rem]      font-[500] leading-[1.5]    tracking-[.0094em]',
    'text-title-small': '    font-body    normal-case text-[.875rem]   font-[500] leading-[1.4286] tracking-[.0071em]',
    'text-body-large': '     font-body    normal-case text-[1rem]      font-[400] leading-[1.5]    tracking-[.0313em]',
    'text-body-medium': '    font-body    normal-case text-[.875rem]   font-[400] leading-[1.4286] tracking-[.0179em]',
    'text-body-small': '     font-body    normal-case text-[.75rem]    font-[400] leading-[1.3333] tracking-[.0333em]',
    'text-label-large': '    font-body    normal-case text-[.875rem]   font-[500] leading-[1.4286] tracking-[.0071em]',
    'text-label-medium': '   font-body    normal-case text-[.75rem]    font-[500] leading-[1.3333] tracking-[.0417em]',
    'text-label-small': '    font-body    normal-case text-[.6875rem]  font-[500] leading-[1.4545] tracking-[.0455em]',
  },
})
