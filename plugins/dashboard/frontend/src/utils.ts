export function sampleBins(data: number[], count: number): number[] {
  if (data.length === 0) return new Array(count).fill(0)
  const maxBin = Math.min(data.length, 24)
  const step = maxBin / count
  return Array.from({ length: count }, (_, i) => {
    const idx = Math.min(Math.round((i + 0.5) * step), data.length - 1)
    return data[idx] ?? 0
  })
}
