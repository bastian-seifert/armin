interface SliderProps {
  min: number
  max: number
  step?: number
  value: number
  onChange: (value: number) => void
  className?: string
}

export function Slider({ min, max, step = 1, value, onChange, className = '' }: SliderProps) {
  return (
    <input
      type="range"
      min={min}
      max={max}
      step={step}
      value={value}
      onChange={(e) => onChange(Number(e.target.value))}
      className={`w-full h-1.5 bg-gray-700 rounded-full appearance-none cursor-pointer
        accent-indigo-500
        [&::-webkit-slider-thumb]:appearance-none
        [&::-webkit-slider-thumb]:w-3
        [&::-webkit-slider-thumb]:h-3
        [&::-webkit-slider-thumb]:rounded-full
        [&::-webkit-slider-thumb]:bg-indigo-500
        [&::-webkit-slider-thumb]:cursor-pointer
        ${className}`}
    />
  )
}
