import { AnimatePresence } from 'motion/react'
import { useEffect, useRef } from 'react'
import type { FeedEntry, TransitionEntry, AgentEvent } from '../../types'
import { useGraph } from '../../context/GraphContext'
import { SessionBanner } from './SessionBanner'
import { EventCard } from './EventCard'

function isTransition(e: FeedEntry): e is TransitionEntry {
  return (e as TransitionEntry).type === 'transition'
}

export function Feed() {
  const { state } = useGraph()
  const { feedEntries: entries, highlightedUtteranceIds: highlightedIds } = state
  const bottomRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [entries.length])

  return (
    <div className="h-full overflow-y-auto flex flex-col gap-2 p-3 scrollbar-thin scrollbar-thumb-gray-700">
      <AnimatePresence initial={false}>
        {entries.map((entry, i) =>
          isTransition(entry) ? (
            <SessionBanner
              key={`transition-${entry.session_id}-${i}`}
              session_id={entry.session_id}
              session_type={entry.session_type}
            />
          ) : (
            <EventCard
              key={(entry as AgentEvent).id}
              event={entry as AgentEvent}
              highlighted={highlightedIds.has((entry as AgentEvent).id)}
            />
          ),
        )}
      </AnimatePresence>
      <div ref={bottomRef} />
    </div>
  )
}
