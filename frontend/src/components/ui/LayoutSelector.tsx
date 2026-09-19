import { Computer, LineChart, BarChart3, Building2 } from 'lucide-react';

export type LayoutMode = 'live' | 'review' | 'executive' | 'org';

interface Props {
  mode: LayoutMode;
  onChange: (mode: LayoutMode) => void;
}

const OPTIONS: { mode: LayoutMode; label: string; icon: typeof Computer; shortcut: string }[] = [
  { mode: 'live', label: 'Live', icon: Computer, shortcut: '1' },
  { mode: 'review', label: 'Review', icon: LineChart, shortcut: '2' },
  { mode: 'executive', label: 'Executive', icon: BarChart3, shortcut: '3' },
  { mode: 'org', label: 'Org', icon: Building2, shortcut: '4' },
];

export function LayoutSelector({ mode, onChange }: Props) {
  return (
    <div className="flex items-center gap-1 bg-gray-900 border border-gray-800 rounded-lg p-0.5">
      {OPTIONS.map(opt => {
        const active = mode === opt.mode;
        const Icon = opt.icon;
        return (
          <button
            key={opt.mode}
            onClick={() => onChange(opt.mode)}
            title={`${opt.label} view (${opt.shortcut})`}
            className={`flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs font-medium transition-colors ${
              active
                ? 'bg-gray-800 text-white'
                : 'text-gray-500 hover:text-gray-300'
            }`}
          >
            <Icon className="w-3.5 h-3.5" />
            <span className="hidden sm:inline">{opt.label}</span>
          </button>
        );
      })}
    </div>
  );
}