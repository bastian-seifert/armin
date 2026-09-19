import type { Community } from '../../types'
import { COMMUNITY_COLORS } from '../../lib/colors'

interface Props {
  communities: Community[]
  onToggleColorMode: () => void
  communityColorMode: boolean
}

export function CommunityLegend({ communities, onToggleColorMode, communityColorMode }: Props) {
  if (communities.length === 0) return null

  const sorted = [...communities].sort((a, b) => b.size - a.size)

  return (
    <div className="px-3 py-2 bg-gray-900/90 border-b border-gray-800 text-xs">
      <div className="flex items-center gap-3 mb-1.5">
        <label className="flex items-center gap-1.5 cursor-pointer select-none">
          <input
            type="checkbox"
            checked={communityColorMode}
            onChange={onToggleColorMode}
            className="rounded border-gray-600 bg-gray-800 text-blue-500 focus:ring-blue-500"
          />
          <span className="text-gray-300 font-medium">Color by community</span>
        </label>
        <span className="text-gray-600">|</span>
        <span className="text-gray-500">
          {sorted.length} communities
        </span>
      </div>
      <div className="flex flex-wrap gap-2">
        {sorted.slice(0, 8).map((c) => {
          const colorIndex = parseInt(c.id) % COMMUNITY_COLORS.length
          const color = COMMUNITY_COLORS[colorIndex]
          return (
            <div key={c.id} className="flex items-center gap-1">
              <div className="w-2 h-2 rounded-full" style={{ backgroundColor: color }} />
              <span className="text-gray-400 truncate max-w-[100px]" title={c.label}>
                {c.label}
              </span>
              <span className="text-gray-600">({c.size})</span>
            </div>
          )
        })}
        {sorted.length > 8 && (
          <span className="text-gray-600">+{sorted.length - 8} more</span>
        )}
      </div>
    </div>
  )
}
