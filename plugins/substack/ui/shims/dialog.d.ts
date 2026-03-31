import type { HTMLAttributes, ReactNode } from 'react'

export declare function Dialog(props: {
  open?: boolean
  onOpenChange?: (open: boolean) => void
  children?: ReactNode
}): JSX.Element
export declare function DialogPortal(props: { children?: ReactNode }): JSX.Element
export declare function DialogOverlay(props: HTMLAttributes<HTMLDivElement>): JSX.Element
export declare function DialogClose(props: HTMLAttributes<HTMLButtonElement>): JSX.Element
export declare function DialogTrigger(props: HTMLAttributes<HTMLButtonElement>): JSX.Element
export declare function DialogContent(props: HTMLAttributes<HTMLDivElement>): JSX.Element
export declare function DialogHeader(props: HTMLAttributes<HTMLDivElement>): JSX.Element
export declare function DialogFooter(props: HTMLAttributes<HTMLDivElement>): JSX.Element
export declare function DialogTitle(props: HTMLAttributes<HTMLHeadingElement>): JSX.Element
export declare function DialogDescription(props: HTMLAttributes<HTMLParagraphElement>): JSX.Element
