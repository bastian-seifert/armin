import type { ReactNode } from 'react'

type BadgeVariant = 'default' | 'success' | 'warning' | 'error' | 'info'

interface BadgeProps {
  variant?: BadgeVariant
  children: ReactNode
  className?: string
}

const variantClasses: Record<BadgeVariant, string> = {
  default: 'bg-gray-700 text-gray-200',
  success: 'bg-emerald-900/60 text-emerald-300',
  warning: 'bg-amber-900/60 text-amber-300',
  error: 'bg-red-900/60 text-red-300',
  info: 'bg-blue-900/60 text-blue-300',
}

export function Badge({ variant = 'default', children, className = '' }: BadgeProps) {
  return (
    <span
      className={`inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium
        ${variantClasses[variant]} ${className}`}
    >
      {children}
    </span>
  )
}
