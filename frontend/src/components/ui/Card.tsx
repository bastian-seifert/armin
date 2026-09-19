import type { ReactNode } from 'react'

interface CardProps {
  title?: string
  children: ReactNode
  className?: string
}

export function Card({ title, children, className = '' }: CardProps) {
  return (
    <div className={`rounded-lg border border-gray-700 bg-gray-900/80 ${className}`}>
      {title && (
        <div className="border-b border-gray-700 px-3 py-2">
          <h3 className="text-sm font-semibold text-gray-200">{title}</h3>
        </div>
      )}
      <div className="p-3">{children}</div>
    </div>
  )
}
