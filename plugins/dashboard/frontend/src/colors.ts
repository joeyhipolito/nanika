/**
 * Design token constants — mirrors CSS custom properties in index.css.
 * Use in JS contexts that cannot reference CSS vars directly:
 * Recharts fill/stroke props, motion inline styles, ReactFlow node styles.
 */

// Neutrals (warm sepia palette) — matches index.css :root
export const neutral = {
  pillBg: '#242422',
  pillBorder: '#3b3b36',
  canvasBg: '#1a1a18',
  textPrimary: '#ccc9c0',
  textSecondary: '#76766e',
  micBg: '#353530',
} as const

export const brand = {
  primary: '#d97757',
  primaryBg: 'rgba(217, 119, 87, 0.15)',
} as const

export const claude = {
  accent: 'oklch(0.65 0.20 276)',
  accentBg: 'oklch(0.65 0.20 276 / 0.15)',
  info: 'oklch(0.66 0.16 240)',
  chart1: 'oklch(0.72 0.16 290)',
  chart2: 'oklch(0.84 0.06 305)',
  chart3: 'oklch(0.88 0.04 90)',
  chart4: 'oklch(0.78 0.08 250)',
} as const

// Status colors — OKLCH, matches --color-* vars in index.css
export const status = {
  success: 'oklch(0.73 0.17 145)',
  successBg: 'oklch(0.73 0.17 145 / 0.12)',
  error: 'oklch(0.63 0.19 22)',
  errorBg: 'oklch(0.63 0.19 22 / 0.12)',
  warning: 'oklch(0.77 0.15 80)',
  warningBg: 'oklch(0.77 0.15 80 / 0.10)',
  info: claude.info,
  accent: brand.primary,
  accentBg: brand.primaryBg,
} as const
