import { useEffect, useRef, useState, useCallback } from 'react'
import { motion, AnimatePresence } from 'motion/react'
import { Search, MessageSquareText, GitBranch, AlertTriangle, Scale, BarChart3, Users, X } from 'lucide-react'
import Markdown from 'react-markdown'
import type { LayoutMode } from './LayoutSelector'
import type { QueryResult } from '../../types'

const API_URL = import.meta.env.VITE_API_URL
  ? `${import.meta.env.VITE_API_URL}/query`
  : 'http://localhost:8080/query'

interface QuickAction {
  id: string
  label: string
  description: string
  icon: typeof Search
  mode: LayoutMode
}

const QUICK_ACTIONS: QuickAction[] = [
  { id: 'decisions', label: 'Show Decisions', description: 'View extracted decisions and their status', icon: GitBranch, mode: 'executive' },
  { id: 'risks', label: 'Show Risks', description: 'View detected risks with impact scores', icon: AlertTriangle, mode: 'executive' },
  { id: 'debt', label: 'Show Debt', description: 'View reasoning debt report', icon: Scale, mode: 'executive' },
  { id: 'summary', label: 'Show Summary', description: 'View executive summary', icon: BarChart3, mode: 'executive' },
  { id: 'communities', label: 'Show Communities', description: 'View community detection report', icon: Users, mode: 'executive' },
  { id: 'feed', label: 'Go to Feed', description: 'Switch to the live event feed', icon: MessageSquareText, mode: 'live' },
]

interface Props {
  open: boolean
  onClose: () => void
  onChangeLayout: (mode: LayoutMode) => void
}

export function CommandPalette({ open, onClose, onChangeLayout }: Props) {
  const inputRef = useRef<HTMLInputElement>(null)
  const [query, setQuery] = useState('')
  const [result, setResult] = useState<QueryResult | null>(null)
  const [loading, setLoading] = useState(false)
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    if (open) {
      setQuery('')
      setResult(null)
      setLoading(false)
      setTimeout(() => inputRef.current?.focus(), 50)
    }
  }, [open])

  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [open, onClose])

  const submitQuery = useCallback(async (q: string) => {
    if (!q.trim() || loading) return
    setLoading(true)
    try {
      const resp = await fetch(API_URL, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ question: q, mode: 'deterministic', depth: 3 }),
      })
      const data = (await resp.json()) as QueryResult
      setResult(data)
    } catch {
      setResult({ answer: 'Query failed — is the server running?', trace: [], cited_events: [], mode_used: 'deterministic' })
    } finally {
      setLoading(false)
    }
  }, [loading])

  const handleInputChange = (value: string) => {
    setQuery(value)
    if (debounceRef.current) clearTimeout(debounceRef.current)
    if (value.trim().length < 3) {
      setResult(null)
      return
    }
    debounceRef.current = setTimeout(() => submitQuery(value), 400)
  }

  const handleAction = (action: QuickAction) => {
    onChangeLayout(action.mode)
    onClose()
  }

  const isQuery = query.trim().length >= 3

  return (
    <AnimatePresence>
      {open && (
        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          className="fixed inset-0 z-50 flex items-start justify-center pt-[15vh]"
        >
          <div className="fixed inset-0 bg-black/60" onClick={onClose} />
          <motion.div
            initial={{ opacity: 0, scale: 0.96, y: -8 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.96, y: -8 }}
            transition={{ duration: 0.15 }}
            className="relative w-full max-w-xl bg-gray-900 border border-gray-700 rounded-xl shadow-2xl overflow-hidden"
          >
            <div className="flex items-center gap-3 px-4 py-3 border-b border-gray-800">
              <Search className="w-4 h-4 text-gray-500 shrink-0" />
              <input
                ref={inputRef}
                type="text"
                value={query}
                onChange={e => handleInputChange(e.target.value)}
                placeholder="Ask about the argument graph or type a command…"
                className="flex-1 bg-transparent text-sm text-gray-100 placeholder-gray-600 focus:outline-none"
              />
              {loading && (
                <svg className="animate-spin w-4 h-4 text-gray-500" fill="none" viewBox="0 0 24 24">
                  <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
                  <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
                </svg>
              )}
              <button onClick={onClose} className="text-gray-600 hover:text-gray-400">
                <X className="w-4 h-4" />
              </button>
            </div>

            <div className="max-h-[60vh] overflow-y-auto">
              {result && isQuery && (
                <div className="px-4 py-3 border-b border-gray-800">
                  <div className="flex items-center gap-2 mb-2">
                    <span className="text-xs text-indigo-400 font-semibold uppercase tracking-wider">Answer</span>
                    <span className={`text-xs px-1.5 py-0.5 rounded font-mono ${
                      result.mode_used === 'deterministic'
                        ? 'bg-emerald-600/20 text-emerald-400'
                        : 'bg-amber-600/20 text-amber-400'
                    }`}>
                      {result.mode_used}
                    </span>
                  </div>
                  <div className="text-sm text-gray-200 leading-relaxed prose prose-invert prose-sm max-w-none
                    prose-headings:mt-3 prose-headings:mb-1.5 prose-headings:text-gray-100
                    prose-h3:text-base prose-h3:font-semibold
                    prose-ul:my-1 prose-li:my-0.5
                    prose-p:my-0
                    prose-strong:text-gray-100 prose-strong:font-semibold
                    prose-em:text-gray-400 prose-em:not-italic">
                    <Markdown>{result.answer}</Markdown>
                  </div>
                  {result.trace.length > 0 && (
                    <p className="mt-2 text-xs text-gray-500">
                      Trace: {result.trace.length} nodes highlighted
                    </p>
                  )}
                </div>
              )}

              {!isQuery && (
                <div className="px-4 py-3">
                  <p className="text-xs font-semibold text-gray-500 uppercase tracking-wider mb-2">Quick Actions</p>
                  <div className="grid grid-cols-1 gap-1">
                    {QUICK_ACTIONS.map(action => (
                      <button
                        key={action.id}
                        onClick={() => handleAction(action)}
                        className="flex items-center gap-3 w-full px-3 py-2.5 rounded-lg text-left hover:bg-gray-800 transition-colors group"
                      >
                        <action.icon className="w-4 h-4 text-gray-500 group-hover:text-indigo-400 shrink-0" />
                        <div className="flex-1 min-w-0">
                          <p className="text-sm text-gray-200 group-hover:text-white">{action.label}</p>
                          <p className="text-xs text-gray-500 truncate">{action.description}</p>
                        </div>
                        <span className="text-xs text-gray-600 font-mono shrink-0">
                          {action.mode}
                        </span>
                      </button>
                    ))}
                  </div>
                </div>
              )}
            </div>

            <div className="flex items-center gap-4 px-4 py-2 border-t border-gray-800 bg-gray-950/50">
              <span className="text-xs text-gray-600">
                <kbd className="px-1 py-0.5 rounded bg-gray-800 border border-gray-700 font-mono text-[10px]">↵</kbd>
                {' '}search
              </span>
              <span className="text-xs text-gray-600">
                <kbd className="px-1 py-0.5 rounded bg-gray-800 border border-gray-700 font-mono text-[10px]">esc</kbd>
                {' '}close
              </span>
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>
  )
}
