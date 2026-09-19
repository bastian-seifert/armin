import { motion } from 'motion/react'
import type { AgentEvent } from '../../types'
import { AGENT_COLORS, AGENT_LABELS } from '../../lib/colors'

interface Props {
  event: AgentEvent
  highlighted: boolean
}

export function EventCard({ event, highlighted }: Props) {
  const colorClass = AGENT_COLORS[event.agent_role] ?? 'bg-gray-900/60 border-gray-700'
  const label = AGENT_LABELS[event.agent_role] ?? event.agent_role
  const t = event.start_time
  const mins = Math.floor(t / 60)
  const secs = Math.floor(t % 60)
  const timeStr = `${mins}:${secs.toString().padStart(2, '0')}`

  return (
    <motion.div
      initial={{ opacity: 0, x: -16 }}
      animate={{ opacity: 1, x: 0 }}
      transition={{ duration: 0.25 }}
      className={`border rounded-lg p-3 text-sm ${colorClass} ${
        highlighted ? 'ring-1 ring-white/30' : ''
      }`}
    >
      <div className="flex justify-between items-baseline mb-1">
        <span className="font-mono text-xs font-semibold text-gray-300">{label}</span>
        <span className="font-mono text-xs text-gray-500">{timeStr}</span>
      </div>
      <p className="text-gray-100 leading-relaxed">{event.text}</p>
    </motion.div>
  )
}
