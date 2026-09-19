import { AnimatePresence, motion } from 'motion/react'
import { useState } from 'react'
import Markdown from 'react-markdown'
import type { QueryResult } from '../../types'

const API_URL = import.meta.env.VITE_API_URL ?? 'http://localhost:8080/query'

type QueryMode = 'deterministic' | 'llm'

interface Props {
  onTraceResult: (nodeIds: Set<string>) => void
}

export function QueryPanel({ onTraceResult }: Props) {
  const [question, setQuestion] = useState('')
  const [result, setResult] = useState<QueryResult | null>(null)
  const [loading, setLoading] = useState(false)
  const [mode, setMode] = useState<QueryMode>('deterministic')
  const [depth, setDepth] = useState(3)

  const submit = async () => {
    if (!question.trim() || loading) return
    setLoading(true)
    try {
      const resp = await fetch(API_URL, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ question, mode, depth }),
      })
      const data = (await resp.json()) as QueryResult
      setResult(data)
      onTraceResult(new Set(data.trace))
    } catch {
      setResult({ answer: 'Query failed — is the server running?', trace: [], cited_events: [], mode_used: mode })
    } finally {
      setLoading(false)
    }
  }

  const clearResult = () => {
    setResult(null)
    onTraceResult(new Set())
  }

  const adjustDepth = (delta: number) => {
    setDepth(prev => Math.min(5, Math.max(1, prev + delta)))
  }

  return (
    <div className="flex-1 flex flex-col gap-2">
      <div className="flex gap-2">
        <input
          type="text"
          value={question}
          onChange={e => setQuestion(e.target.value)}
          onKeyDown={e => e.key === 'Enter' && submit()}
          placeholder="Ask about the argument graph…"
          className="flex-1 bg-gray-800 border border-gray-700 rounded-lg px-3 py-2 text-sm text-gray-100 placeholder-gray-600 focus:outline-none focus:border-indigo-500"
        />
        <button
          onClick={submit}
          disabled={loading || !question.trim()}
          className="px-4 py-2 rounded-lg text-sm font-semibold bg-indigo-600 hover:bg-indigo-500 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
        >
          {loading ? '…' : 'Ask'}
        </button>
      </div>

      <div className="flex items-center gap-3">
        <span className="text-xs text-gray-500">Mode:</span>
        <button
          onClick={() => setMode('deterministic')}
          className={`px-2 py-1 rounded text-xs font-medium transition-colors ${
            mode === 'deterministic'
              ? 'bg-emerald-600/20 text-emerald-400 border border-emerald-600/40'
              : 'bg-gray-800 text-gray-500 border border-gray-700 hover:text-gray-300'
          }`}
        >
          Deterministic
        </button>
        <button
          onClick={() => setMode('llm')}
          className={`px-2 py-1 rounded text-xs font-medium transition-colors ${
            mode === 'llm'
              ? 'bg-amber-600/20 text-amber-400 border border-amber-600/40'
              : 'bg-gray-800 text-gray-500 border border-gray-700 hover:text-gray-300'
          }`}
        >
          LLM
        </button>

        <span className="text-gray-700">|</span>

        <span className="text-xs text-gray-500">Depth:</span>
        <div className="flex items-center gap-1">
          <button
            onClick={() => adjustDepth(-1)}
            disabled={depth <= 1}
            className="w-5 h-5 flex items-center justify-center rounded bg-gray-800 border border-gray-700 text-gray-400 hover:text-gray-200 disabled:opacity-30 disabled:cursor-not-allowed text-xs"
          >
            −
          </button>
          <span className="text-xs text-gray-300 w-4 text-center font-mono">{depth}</span>
          <button
            onClick={() => adjustDepth(1)}
            disabled={depth >= 5}
            className="w-5 h-5 flex items-center justify-center rounded bg-gray-800 border border-gray-700 text-gray-400 hover:text-gray-200 disabled:opacity-30 disabled:cursor-not-allowed text-xs"
          >
            +
          </button>
        </div>
      </div>

      <AnimatePresence>
        {result && (
          <motion.div
            initial={{ opacity: 0, height: 0 }}
            animate={{ opacity: 1, height: 'auto' }}
            exit={{ opacity: 0, height: 0 }}
            className="overflow-hidden"
          >
            <div className="bg-gray-800/60 border border-gray-700 rounded-lg p-3">
              <div className="flex justify-between items-start mb-2">
                <div className="flex items-center gap-2">
                  <span className="text-xs text-indigo-400 font-semibold uppercase tracking-wider">Answer</span>
                  <span className={`text-xs px-1.5 py-0.5 rounded font-mono ${
                    result.mode_used === 'deterministic'
                      ? 'bg-emerald-600/20 text-emerald-400'
                      : 'bg-amber-600/20 text-amber-400'
                  }`}>
                    {result.mode_used}
                  </span>
                </div>
                <button
                  onClick={clearResult}
                  className="text-gray-500 hover:text-gray-300 text-xs"
                >
                  clear trace
                </button>
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
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  )
}
