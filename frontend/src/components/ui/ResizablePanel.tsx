import { useRef, useState, useCallback, useEffect } from 'react'
import type { ReactNode, PointerEvent } from 'react'

interface ResizablePanelProps {
  left: ReactNode
  right: ReactNode
  defaultRatio?: number
  minLeft?: number
  minRight?: number
}

export function ResizablePanel({
  left,
  right,
  defaultRatio = 0.3,
  minLeft = 100,
  minRight = 100,
}: ResizablePanelProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const [ratio, setRatio] = useState(defaultRatio)
  const dragging = useRef(false)

  const handlePointerDown = useCallback((e: PointerEvent) => {
    e.preventDefault()
    dragging.current = true
    ;(e.target as HTMLElement).setPointerCapture(e.pointerId)
  }, [])

  const handlePointerMove = useCallback((e: PointerEvent) => {
    if (!dragging.current || !containerRef.current) return
    const rect = containerRef.current.getBoundingClientRect()
    const x = e.clientX - rect.left
    const newRatio = Math.max(minLeft / rect.width, Math.min(1 - minRight / rect.width, x / rect.width))
    setRatio(newRatio)
  }, [minLeft, minRight])

  const handlePointerUp = useCallback(() => {
    dragging.current = false
  }, [])

  useEffect(() => {
    const onUp = () => { dragging.current = false }
    window.addEventListener('pointerup', onUp)
    return () => window.removeEventListener('pointerup', onUp)
  }, [])

  return (
    <div
      ref={containerRef}
      className="flex flex-1 overflow-hidden"
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
    >
      <div style={{ width: `${ratio * 100}%` }} className="overflow-hidden flex flex-col">
        {left}
      </div>
      <div
        className="w-1 bg-gray-800 hover:bg-indigo-500 cursor-col-resize shrink-0 relative transition-colors"
        onPointerDown={handlePointerDown}
      >
        <div className="absolute inset-y-0 -left-1 -right-1" />
      </div>
      <div style={{ width: `${(1 - ratio) * 100}%` }} className="overflow-hidden flex flex-col">
        {right}
      </div>
    </div>
  )
}
