import type { TextareaHTMLAttributes } from 'react'

interface TextareaProps extends TextareaHTMLAttributes<HTMLTextAreaElement> {
  maxLength?: number
}

export function Textarea({ className = '', maxLength, ...props }: TextareaProps) {
  return (
    <textarea
      className={`w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2
        text-sm text-gray-200 placeholder-gray-500
        focus:border-indigo-500 focus:outline-none focus:ring-1 focus:ring-indigo-500
        resize-none transition-colors
        ${className}`}
      maxLength={maxLength}
      {...props}
    />
  )
}
