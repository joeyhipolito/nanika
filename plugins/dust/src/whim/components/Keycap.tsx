import type { ReactNode } from 'react'

export type KeycapVariant = 'add' | 'rem' | 'acc' | 'nav'

interface KeycapProps {
  children: ReactNode
  variant?: KeycapVariant
}

export function Keycap({ children, variant }: KeycapProps) {
  return (
    <span className={variant ? `K ${variant}` : 'K'}>
      {children}
    </span>
  )
}
