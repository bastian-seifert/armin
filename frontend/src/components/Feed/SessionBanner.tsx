import { motion } from 'motion/react'

interface Props {
  session_id: string
  session_type: string
}

export function SessionBanner({ session_id, session_type }: Props) {
  return (
    <motion.div
      initial={{ opacity: 0, scaleX: 0.8 }}
      animate={{ opacity: 1, scaleX: 1 }}
      transition={{ duration: 0.4 }}
      className="my-2 py-2 px-4 text-center rounded-lg bg-indigo-950/80 border border-indigo-700 text-indigo-300 font-semibold text-xs tracking-widest uppercase"
    >
      {session_id}: {session_type}
    </motion.div>
  )
}
