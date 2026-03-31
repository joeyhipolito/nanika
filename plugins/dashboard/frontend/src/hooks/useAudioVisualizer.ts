import { useState, useRef, useCallback, useEffect } from 'react'

export function useAudioVisualizer() {
  const [frequencyData, setFrequencyData] = useState<number[]>([])
  const [micUnavailable, setMicUnavailable] = useState(false)

  const sessionRef = useRef(0)
  const audioContextRef = useRef<AudioContext | null>(null)
  const sourceRef = useRef<MediaStreamAudioSourceNode | null>(null)
  const streamRef = useRef<MediaStream | null>(null)
  const rafIdRef = useRef(0)

  const teardown = useCallback(() => {
    if (rafIdRef.current) {
      cancelAnimationFrame(rafIdRef.current)
      rafIdRef.current = 0
    }
    sourceRef.current?.disconnect()
    sourceRef.current = null
    streamRef.current?.getTracks().forEach(t => t.stop())
    streamRef.current = null
    setFrequencyData([])
  }, [])

  const stop = useCallback(() => {
    sessionRef.current++
    teardown()
  }, [teardown])

  const start = useCallback(async () => {
    teardown()
    const session = ++sessionRef.current
    setMicUnavailable(false)

    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true })

      if (session !== sessionRef.current) {
        stream.getTracks().forEach(t => t.stop())
        return
      }

      streamRef.current = stream

      if (!audioContextRef.current || audioContextRef.current.state === 'closed') {
        audioContextRef.current = new AudioContext()
      }
      const ctx = audioContextRef.current
      if (ctx.state === 'suspended') await ctx.resume()

      const analyser = ctx.createAnalyser()
      analyser.fftSize = 256
      analyser.smoothingTimeConstant = 0.6

      const source = ctx.createMediaStreamSource(stream)
      sourceRef.current = source
      source.connect(analyser)

      const dataArray = new Uint8Array(analyser.frequencyBinCount)
      let frame = 0
      const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches

      const tick = () => {
        if (session !== sessionRef.current) return
        analyser.getByteFrequencyData(dataArray)
        if (!reducedMotion && ++frame % 3 === 0) {
          setFrequencyData(Array.from(dataArray, v => v / 255))
        }
        rafIdRef.current = requestAnimationFrame(tick)
      }

      rafIdRef.current = requestAnimationFrame(tick)
    } catch (err) {
      console.warn('Audio visualizer:', err)
      if (session === sessionRef.current) {
        setMicUnavailable(true)
      }
    }
  }, [teardown])

  useEffect(() => {
    return () => {
      stop()
      const audioContext = audioContextRef.current
      audioContextRef.current = null
      audioContext?.close()
    }
  }, [stop])

  return { frequencyData, micUnavailable, start, stop, streamRef }
}
