import { useEffect } from 'react'
import { AnimatePresence, motion } from 'motion/react'
import { status } from '../colors'

export type ToastType = 'info' | 'success' | 'warning' | 'error'

export interface Toast {
  id: string
  text: string
  type: ToastType
}

const MAX_VISIBLE = 5

const TYPE_BORDER: Record<ToastType, string> = {
  info: status.info,
  success: status.success,
  warning: status.warning,
  error: status.error,
}

interface ToastItemProps {
  toast: Toast
  onDismiss: (id: string) => void
}

function ToastItem({ toast, onDismiss }: ToastItemProps) {
  useEffect(() => {
    const timer = setTimeout(() => onDismiss(toast.id), 4000)
    return () => clearTimeout(timer)
  }, [toast.id, onDismiss])

  return (
    <motion.div
      className="toast"
      style={{ borderLeft: `3px solid ${TYPE_BORDER[toast.type]}` }}
      initial={{ opacity: 0, y: 8, scale: 0.97 }}
      animate={{ opacity: 1, y: 0, scale: 1 }}
      exit={{ opacity: 0, y: 4, scale: 0.97 }}
      transition={{ duration: 0.15 }}
      onClick={() => onDismiss(toast.id)}
      layout
    >
      {toast.text}
    </motion.div>
  )
}

interface ToastStackProps {
  toasts: Toast[]
  onDismiss: (id: string) => void
}

export function ToastStack({ toasts, onDismiss }: ToastStackProps) {
  const visible = toasts.slice(0, MAX_VISIBLE)
  const queued = toasts.length - visible.length

  return (
    <div className="toast-stack" role="log" aria-live="polite" aria-label="Notifications">
      <AnimatePresence initial={false}>
        {visible.map((t) => (
          <ToastItem key={t.id} toast={t} onDismiss={onDismiss} />
        ))}
        {queued > 0 && (
          <motion.div
            key="toast-queue-count"
            className="toast toast--queue-count"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.15 }}
          >
            +{queued} more
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  )
}
