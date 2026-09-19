import { motion } from 'motion/react'
import type { ArgumentNode, CommunityReport } from '../../types'
import { COMMUNITY_COLORS, NODE_COLORS } from '../../lib/colors'

interface Props {
  node: ArgumentNode
  onClose: () => void
  communityReport?: CommunityReport | null
}

export function NodeDetail({ node, onClose, communityReport }: Props) {
  const color = NODE_COLORS[node.node_type]

  const communityInfo = communityReport?.communities.find(c =>
    c.node_ids.includes(node.id),
  )
  const communityColor = communityInfo
    ? COMMUNITY_COLORS[parseInt(communityInfo.id) % COMMUNITY_COLORS.length]
    : undefined

  return (
    <motion.div
      initial={{ opacity: 0, x: 40 }}
      animate={{ opacity: 1, x: 0 }}
      exit={{ opacity: 0, x: 40 }}
      transition={{ duration: 0.2 }}
      className="absolute top-4 right-4 w-72 bg-gray-900 border border-gray-700 rounded-xl p-4 shadow-2xl z-20"
    >
      <div className="flex justify-between items-start mb-3">
        <span
          className="text-xs font-bold px-2 py-1 rounded-full"
          style={{ backgroundColor: color + '30', color }}
        >
          {node.node_type}
        </span>
        <button
          onClick={onClose}
          className="text-gray-500 hover:text-gray-200 text-lg leading-none ml-2"
          aria-label="Close"
        >
          ×
        </button>
      </div>
      <p className="text-sm font-semibold text-white mb-2">{node.label}</p>
      <p className="text-xs text-gray-300 leading-relaxed mb-3">{node.description}</p>
      <div className="text-xs text-gray-500 space-y-1">
        <div>Agent: <span className="text-gray-300">{node.agent_id}</span></div>
        <div>Session: <span className="text-gray-300">{node.session_id}</span></div>
        <div>Confidence: <span className="text-gray-300">{(node.confidence * 100).toFixed(0)}%</span></div>
        {communityInfo && (
          <div className="flex items-center gap-1.5">
            <span>Community:</span>
            <span
              className="w-2 h-2 rounded-full inline-block"
              style={{ backgroundColor: communityColor }}
            />
            <span className="text-gray-300">{communityInfo.label}</span>
          </div>
        )}
      </div>
    </motion.div>
  )
}
