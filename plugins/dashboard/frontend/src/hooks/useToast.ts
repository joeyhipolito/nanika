import { useCallback, useRef, useState } from 'react'
import type { Toast, ToastType } from '../components/Toast'

export function useToast() {
  const [queue, setQueue] = useState<Toast[]>([])
  const counterRef = useRef(0)

  const addToast = useCallback((text: string, type: ToastType = 'info') => {
    const id = `toast-${++counterRef.current}`
    setQueue((prev) => [...prev, { id, text, type }])
  }, [])

  const dismissToast = useCallback((id: string) => {
    setQueue((prev) => prev.filter((t) => t.id !== id))
  }, [])

  return { toasts: queue, addToast, dismissToast }
}
