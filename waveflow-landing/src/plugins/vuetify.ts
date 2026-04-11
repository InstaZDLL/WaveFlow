/**
 * plugins/vuetify.ts
 *
 * Framework documentation: https://vuetifyjs.com
 */

import { createVuetify } from 'vuetify'
import { forVuetify } from '../theme/breakpoints'

import '@mdi/font/css/materialdesignicons.css'
import '../styles/layers.css'
import 'vuetify/styles'

export default createVuetify({
  theme: {
    defaultTheme: 'waveflowDark',
    themes: {
      waveflowDark: {
        dark: true,
        colors: {
          background: '#0A0A0A',
          surface: '#121212',
          'surface-variant': '#181818',
          primary: '#10B981',
          'primary-darken-1': '#059669',
          secondary: '#34D399',
          error: '#EF4444',
          info: '#3B82F6',
          success: '#10B981',
          warning: '#F59E0B',
          'on-background': '#E5E7EB',
          'on-surface': '#E5E7EB',
        },
      },
    },
  },
  display: {
    mobileBreakpoint: 'md',
    thresholds: forVuetify,
  },
})
