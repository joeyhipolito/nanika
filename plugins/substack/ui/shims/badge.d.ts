import type { HTMLAttributes } from 'react'

export interface BadgeProps extends HTMLAttributes<HTMLDivElement> {
  variant?: 'default' | 'secondary' | 'destructive' | 'outline'
}
export declare function Badge(props: BadgeProps): JSX.Element
export declare function badgeVariants(props?: {
  variant?: BadgeProps['variant']
  className?: string
}): string
