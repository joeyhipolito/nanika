import { useCallback, useEffect, useRef, useState } from 'react'
import { useAudioVisualizer } from './useAudioVisualizer'

interface SpeechRecognitionAlternativeLike {
  transcript: string
}

interface SpeechRecognitionResultLike {
  isFinal: boolean
  length: number
  [index: number]: SpeechRecognitionAlternativeLike
}

interface SpeechRecognitionResultListLike {
  length: number
  [index: number]: SpeechRecognitionResultLike
}

interface SpeechRecognitionEventLike {
  resultIndex: number
  results: SpeechRecognitionResultListLike
}

interface SpeechRecognitionErrorEventLike {
  error: string
}

interface SpeechRecognitionLike {
  continuous: boolean
  interimResults: boolean
  lang: string
  onresult: ((event: SpeechRecognitionEventLike) => void) | null
  onerror: ((event: SpeechRecognitionErrorEventLike) => void) | null
  onend: (() => void) | null
  start: () => void
  stop: () => void
  abort: () => void
}

interface SpeechRecognitionWindow extends Window {
  SpeechRecognition?: new () => SpeechRecognitionLike
  webkitSpeechRecognition?: new () => SpeechRecognitionLike
}

interface UseVoiceInputOptions {
  onInterimTranscript: (text: string) => void
  onFinalTranscript: (text: string) => void
}

function normalizeTranscript(text: string): string {
  return text.replace(/\s+/g, ' ').trim()
}

function getRecognitionCtor(): (new () => SpeechRecognitionLike) | null {
  if (typeof window === 'undefined') return null
  const voiceWindow = window as SpeechRecognitionWindow
  return voiceWindow.SpeechRecognition ?? voiceWindow.webkitSpeechRecognition ?? null
}

export function useVoiceInput(options: UseVoiceInputOptions) {
  const optionsRef = useRef(options)
  optionsRef.current = options

  const [listening, setListening] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const recognitionRef = useRef<SpeechRecognitionLike | null>(null)
  const finalTranscriptRef = useRef('')
  const interimTranscriptRef = useRef('')

  const { frequencyData, micUnavailable, start: startVisualizer, stop: stopVisualizer } = useAudioVisualizer()

  const supported = getRecognitionCtor() !== null

  const stop = useCallback(() => {
    recognitionRef.current?.stop()
  }, [])

  const resetSession = useCallback(() => {
    finalTranscriptRef.current = ''
    interimTranscriptRef.current = ''
  }, [])

  const finishSession = useCallback(() => {
    stopVisualizer()
    setListening(false)
    const transcript = normalizeTranscript([finalTranscriptRef.current, interimTranscriptRef.current].join(' '))
    recognitionRef.current = null
    resetSession()
    if (transcript) {
      optionsRef.current.onFinalTranscript(transcript)
    }
  }, [resetSession, stopVisualizer])

  const start = useCallback(() => {
    const RecognitionCtor = getRecognitionCtor()
    if (!RecognitionCtor) {
      setError('Voice input is not supported in this browser.')
      return
    }

    recognitionRef.current?.abort()
    resetSession()
    setError(null)
    setListening(true)
    void startVisualizer()

    const recognition = new RecognitionCtor()
    recognition.continuous = true
    recognition.interimResults = true
    recognition.lang = 'en-US'

    recognition.onresult = (event: SpeechRecognitionEventLike) => {
      let finalText = finalTranscriptRef.current
      let interimText = ''

      for (let i = event.resultIndex; i < event.results.length; i++) {
        const result = event.results[i]
        const alt = result[0]
        if (!alt?.transcript) continue
        if (result.isFinal) {
          finalText = normalizeTranscript([finalText, alt.transcript].join(' '))
        } else {
          interimText = normalizeTranscript([interimText, alt.transcript].join(' '))
        }
      }

      finalTranscriptRef.current = finalText
      interimTranscriptRef.current = interimText
      optionsRef.current.onInterimTranscript(
        normalizeTranscript([finalText, interimText].join(' ')),
      )
    }

    recognition.onerror = (event: SpeechRecognitionErrorEventLike) => {
      if (event.error === 'no-speech') return
      const nextError =
        event.error === 'not-allowed'
          ? 'Microphone access was denied.'
          : 'Voice input failed.'
      setError(nextError)
    }

    recognition.onend = () => {
      finishSession()
    }

    recognition.start()
    recognitionRef.current = recognition
  }, [finishSession, resetSession, startVisualizer])

  const toggle = useCallback(() => {
    if (listening) {
      stop()
      return
    }
    start()
  }, [listening, start, stop])

  useEffect(() => {
    return () => {
      recognitionRef.current?.abort()
      stopVisualizer()
    }
  }, [stopVisualizer])

  return {
    supported,
    listening,
    error: micUnavailable ? 'Microphone access was denied.' : error,
    frequencyData,
    toggle,
    stop,
  }
}
