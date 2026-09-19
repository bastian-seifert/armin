export interface BBox {
  x: number
  y: number
  w: number
  h: number
}

/** Scale a base font size by the zoom transform so text stays readable when zoomed out. */
export function clampedFont(k: number, basePx: number): string {
  return `${Math.max(basePx * k, 6)}px sans-serif`
}

/** Scale a base stroke width by the zoom transform, clamped to a minimum. */
export function clampedStroke(k: number, basePx: number): number {
  return Math.max(basePx * k, 0.5)
}

/** Scale a dash array by the zoom transform. */
export function clampedDash(k: number, dash: number[]): number[] {
  return dash.map(d => d * k)
}

/** Compute a zoom transform that fits the given bounding box into the viewport. */
export function fitTransform(
  bbox: BBox,
  viewport: { w: number; h: number },
  padding: number = 40,
): { x: number; y: number; k: number } {
  const bw = bbox.w + padding * 2
  const bh = bbox.h + padding * 2
  const k = Math.min(viewport.w / bw, viewport.h / bh, 2)
  const cx = bbox.x + bbox.w / 2
  const cy = bbox.y + bbox.h / 2
  return {
    x: viewport.w / 2 - cx * k,
    y: viewport.h / 2 - cy * k,
    k,
  }
}

/** Create a simple spring (lerp) for smooth position interpolation. */
export interface Spring {
  value: number
  target: number
  set(v: number): void
  step(dt: number): number
}

export function makeSpring(initial: number, _stiffness: number = 0.12): Spring {
  return {
    value: initial,
    target: initial,
    set(v: number) { this.target = v },
    step(dt: number) {
      this.value += (this.target - this.value) * Math.min(1, 0.12 * dt * 60)
      return this.value
    },
  }
}
