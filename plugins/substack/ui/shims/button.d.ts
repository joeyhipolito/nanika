import type { ButtonHTMLAttributes } from 'react'

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: 'default' | 'destructive' | 'outline' | 'secondary' | 'ghost' | 'link'
  size?: 'default' | 'sm' | 'lg' | 'icon'
  asChild?: boolean
}
export declare function Button(props: ButtonProps): JSX.Element
export declare function buttonVariants(props?: {
  variant?: ButtonProps['variant']
  size?: ButtonProps['size']
  className?: string
}): string
