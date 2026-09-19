import { useState, useRef } from 'react'
import type { ReactNode } from 'react'

interface TooltipProps {
  content: string
  children: ReactNode
  position?: 'top' | 'bottom'
}

export function Tooltip({ content, children, position = 'top' }: TooltipProps) {
  const [show, setShow] = useState(false)
  const timeoutRef = useRef<ReturnType<typeof setTimeout>>(undefined)

  const posClasses = position === 'top'
    ? 'bottom-full mb-2'
    : 'top-full mt-2'

  return (
    <div
      className="relative inline-flex"
      onMouseEnter={() => {
        clearTimeout(timeoutRef.current)
        setShow(true)
      }}
      onMouseLeave={() => {
        timeoutRef.current = setTimeout(() => setShow(false), 100)
      }}
    >
      {children}
      {show && (
        <div
          className={`absolute left-1/2 -translate-x-1/2 z-50 px-2 py-1
            text-xs text-white bg-gray-800 rounded
            shadow-lg whitespace-nowrap pointer-events-none
            ${posClasses}`}
        >
          {content}
        </div>
      )}
    </div>
  )
}
