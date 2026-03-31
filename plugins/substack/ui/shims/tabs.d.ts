import type { ComponentPropsWithoutRef, ElementRef, HTMLAttributes } from 'react'

export declare function Tabs(props: {
  defaultValue?: string
  value?: string
  onValueChange?: (value: string) => void
  children?: React.ReactNode
  className?: string
  [key: string]: unknown
}): JSX.Element
export declare function TabsList(props: HTMLAttributes<HTMLDivElement>): JSX.Element
export declare function TabsTrigger(
  props: HTMLAttributes<HTMLButtonElement> & { value: string; disabled?: boolean }
): JSX.Element
export declare function TabsContent(
  props: HTMLAttributes<HTMLDivElement> & { value: string }
): JSX.Element
