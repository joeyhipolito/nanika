export type ShellScale = 'pill' | 'palette' | 'detail' | 'canvas'

/**
 * Step back one rung on the Esc ladder.
 * Returns null when already at the bottom (pill → dismiss).
 */
export function escStep(scale: ShellScale): ShellScale | null {
  switch (scale) {
    case 'canvas':  return 'detail'
    case 'detail':  return 'palette'
    case 'palette': return 'pill'
    case 'pill':    return null
  }
}

export const SCALE_LABEL: Record<ShellScale, string> = {
  pill:    'L0 · Pill',
  palette: 'L1 · Palette',
  detail:  'L2 · Detail',
  canvas:  'L3 · Canvas',
}
