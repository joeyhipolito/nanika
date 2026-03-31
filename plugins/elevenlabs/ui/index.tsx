import { useState, useEffect, useCallback } from 'react'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { queryPluginStatus, queryPluginItems, pluginAction } from '@/lib/wails'
import type { PluginViewProps } from '@/types'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface Voice {
  id: string
  name: string
  category?: string
  preview_url?: string
  accent?: string
}

interface GeneratedAudio {
  id: string
  text: string
  voice_id: string
  voice_name: string
  created_at: string
  duration?: number
  audio_url?: string
}

interface QuotaInfo {
  character_limit: number
  character_count: number
  remaining: number
}

interface ElevenlabsStatus {
  quota: QuotaInfo
  voices_count: number
  recent_count: number
}

type AudioFormat = 'mp3_44100_128' | 'mp3_44100_192' | 'opus_48000_32'
type TabView = 'synthesize' | 'transcribe' | 'history'

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const PLUGIN_NAME = 'elevenlabs'
const MAX_CHAR_LIMIT = 5000
const AUDIO_FORMATS: { label: string; value: AudioFormat }[] = [
  { label: 'MP3 (128kbps)', value: 'mp3_44100_128' },
  { label: 'MP3 (192kbps)', value: 'mp3_44100_192' },
  { label: 'Opus (32kbps)', value: 'opus_48000_32' },
]

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatDate(dateStr: string): string {
  try {
    const d = new Date(dateStr)
    const now = new Date()
    const isToday =
      d.getDate() === now.getDate() &&
      d.getMonth() === now.getMonth() &&
      d.getFullYear() === now.getFullYear()
    if (isToday) {
      return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    }
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' })
  } catch {
    return dateStr
  }
}

function getQuotaPercentage(quota: QuotaInfo): number {
  if (quota.character_limit === 0) return 0
  return Math.min((quota.character_count / quota.character_limit) * 100, 100)
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function VoicePicker({
  voices,
  selectedId,
  onSelect,
  loading,
}: {
  voices: Voice[]
  selectedId: string
  onSelect: (id: string) => void
  loading: boolean
}) {
  if (voices.length === 0) {
    return (
      <div className="text-center py-6">
        <p className="text-sm" style={{ color: 'var(--text-secondary)' }}>
          {loading ? 'Loading voices...' : 'No voices available'}
        </p>
      </div>
    )
  }

  return (
    <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-4">
      {voices.map(voice => (
        <button
          key={voice.id}
          onClick={() => onSelect(voice.id)}
          className="p-2.5 rounded-md border transition-all text-left"
          style={{
            background: selectedId === voice.id
              ? 'color-mix(in srgb, var(--accent) 15%, transparent)'
              : 'var(--mic-bg)',
            borderColor: selectedId === voice.id ? 'var(--accent)' : 'var(--pill-border)',
            color: 'var(--text-primary)',
          }}
        >
          <p className="text-xs font-medium truncate">{voice.name}</p>
          {voice.accent && (
            <p className="text-[10px] mt-0.5" style={{ color: 'var(--text-secondary)' }}>
              {voice.accent}
            </p>
          )}
        </button>
      ))}
    </div>
  )
}

function TabButton({
  label,
  isActive,
  onClick,
}: {
  label: string
  isActive: boolean
  onClick: () => void
}) {
  return (
    <button
      onClick={onClick}
      className="px-3 py-2 text-xs font-medium rounded-md transition-colors"
      style={{
        background: isActive ? 'color-mix(in srgb, var(--accent) 15%, transparent)' : 'var(--mic-bg)',
        color: isActive ? 'var(--accent)' : 'var(--text-secondary)',
        borderBottom: isActive ? '2px solid var(--accent)' : '2px solid transparent',
      }}
    >
      {label}
    </button>
  )
}

function QuotaProgressBar({ quota }: { quota: QuotaInfo }) {
  const percentage = getQuotaPercentage(quota)
  const isNearLimit = percentage > 80

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between">
        <label className="text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
          Character Quota
        </label>
        <span
          className="text-xs"
          style={{
            color: isNearLimit ? 'var(--color-warning)' : 'var(--text-secondary)',
          }}
        >
          {quota.character_count} / {quota.character_limit}
        </span>
      </div>
      <div
        className="h-2 rounded-full overflow-hidden"
        style={{ background: 'var(--pill-border)' }}
        role="progressbar"
        aria-valuenow={percentage}
        aria-valuemin={0}
        aria-valuemax={100}
      >
        <div
          className="h-full transition-all"
          style={{
            width: `${percentage}%`,
            background: isNearLimit ? 'var(--color-warning)' : 'var(--accent)',
          }}
        />
      </div>
    </div>
  )
}

function SynthesizeTab({
  voices,
  quota,
  selectedVoice,
  onVoiceSelect,
  onGenerate,
  isGenerating,
}: {
  voices: Voice[]
  quota: QuotaInfo
  selectedVoice: string
  onVoiceSelect: (id: string) => void
  onGenerate: (text: string, voiceId: string, speed: number, format: AudioFormat) => Promise<void>
  isGenerating: boolean
}) {
  const [text, setText] = useState('')
  const [speed, setSpeed] = useState('1.0')
  const [format, setFormat] = useState<AudioFormat>('mp3_44100_128')
  const [audioUrl, setAudioUrl] = useState<string | null>(null)
  const [isLoading, setIsLoading] = useState(false)

  const charCount = text.length
  const canGenerate = charCount > 0 && charCount <= MAX_CHAR_LIMIT && selectedVoice && !isLoading

  const handleGenerate = async () => {
    if (!canGenerate) return
    setIsLoading(true)
    try {
      await onGenerate(text, selectedVoice, parseFloat(speed), format)
      // In a real app, you'd get back the audio URL
      setAudioUrl(null)
    } finally {
      setIsLoading(false)
    }
  }

  return (
    <div className="space-y-4">
      {/* Voice Selection */}
      <div className="space-y-2">
        <label className="text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
          Select Voice
        </label>
        <VoicePicker
          voices={voices}
          selectedId={selectedVoice}
          onSelect={onVoiceSelect}
          loading={isGenerating}
        />
      </div>

      {/* Quota Bar */}
      <QuotaProgressBar quota={quota} />

      {/* Text Input */}
      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <label
            htmlFor="tts-input"
            className="text-xs font-medium"
            style={{ color: 'var(--text-primary)' }}
          >
            Text to Generate
          </label>
          <span
            className="text-xs"
            style={{
              color: charCount > MAX_CHAR_LIMIT ? 'var(--color-error)' : 'var(--text-secondary)',
            }}
          >
            {charCount} / {MAX_CHAR_LIMIT}
          </span>
        </div>
        <textarea
          id="tts-input"
          value={text}
          onChange={e => setText(e.target.value)}
          placeholder="Enter text to convert to speech..."
          className="w-full p-2.5 rounded-md border text-sm resize-none focus:outline-none focus:ring-1"
          style={{
            background: 'var(--mic-bg)',
            borderColor: charCount > MAX_CHAR_LIMIT ? 'var(--color-error)' : 'var(--pill-border)',
            color: 'var(--text-primary)',
          }}
          rows={5}
          disabled={isLoading}
          maxLength={MAX_CHAR_LIMIT}
        />
      </div>

      {/* Voice Settings */}
      <div className="grid grid-cols-2 gap-3">
        <div className="space-y-2">
          <label htmlFor="speed-input" className="text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
            Speed
          </label>
          <input
            id="speed-input"
            type="range"
            min="0.7"
            max="1.2"
            step="0.1"
            value={speed}
            onChange={e => setSpeed(e.target.value)}
            disabled={isLoading}
            className="w-full"
          />
          <p className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
            {speed}x
          </p>
        </div>

        <div className="space-y-2">
          <label htmlFor="format-select" className="text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
            Format
          </label>
          <select
            id="format-select"
            value={format}
            onChange={e => setFormat(e.target.value as AudioFormat)}
            disabled={isLoading}
            className="w-full p-2 rounded-md border text-xs"
            style={{
              background: 'var(--mic-bg)',
              borderColor: 'var(--pill-border)',
              color: 'var(--text-primary)',
            }}
          >
            {AUDIO_FORMATS.map(fmt => (
              <option key={fmt.value} value={fmt.value}>
                {fmt.label}
              </option>
            ))}
          </select>
        </div>
      </div>

      {/* Generate Button */}
      <Button
        onClick={handleGenerate}
        disabled={!canGenerate}
        className="w-full"
        style={{
          background: canGenerate ? 'var(--accent)' : 'var(--pill-border)',
          color: canGenerate ? 'var(--bg)' : 'var(--text-secondary)',
        }}
      >
        {isLoading ? 'Generating...' : 'Generate Audio'}
      </Button>

      {/* Audio Player */}
      {audioUrl && (
        <Card style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }} className="p-3">
          <audio
            controls
            src={audioUrl}
            className="w-full"
            style={{
              filter: 'invert(1)',
            }}
          />
        </Card>
      )}
    </div>
  )
}

function TranscribeTab({
  onTranscribe,
  isProcessing,
}: {
  onTranscribe: (file: File) => Promise<void>
  isProcessing: boolean
}) {
  const [isDragging, setIsDragging] = useState(false)
  const [selectedFile, setSelectedFile] = useState<File | null>(null)

  const handleDragOver = (e: React.DragEvent) => {
    e.preventDefault()
    setIsDragging(true)
  }

  const handleDragLeave = () => {
    setIsDragging(false)
  }

  const handleDrop = (e: React.DragEvent) => {
    e.preventDefault()
    setIsDragging(false)
    const files = Array.from(e.dataTransfer.files)
    if (files.length > 0) {
      setSelectedFile(files[0])
    }
  }

  const handleFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = e.currentTarget.files
    if (files && files.length > 0) {
      setSelectedFile(files[0])
    }
  }

  const handleTranscribe = async () => {
    if (!selectedFile) return
    try {
      await onTranscribe(selectedFile)
      setSelectedFile(null)
    } catch {
      // Error will be displayed by parent
    }
  }

  return (
    <div className="space-y-4">
      <div
        onDragOver={handleDragOver}
        onDragLeave={handleDragLeave}
        onDrop={handleDrop}
        className="flex flex-col items-center justify-center p-8 rounded-md border-2 border-dashed transition-colors cursor-pointer"
        style={{
          borderColor: isDragging ? 'var(--accent)' : 'var(--pill-border)',
          background: isDragging ? 'color-mix(in srgb, var(--accent) 5%, transparent)' : 'var(--mic-bg)',
        }}
      >
        <p className="text-sm font-medium mb-2" style={{ color: 'var(--text-primary)' }}>
          Drop audio file here
        </p>
        <p className="text-xs mb-3" style={{ color: 'var(--text-secondary)' }}>
          or click to select
        </p>
        <input
          type="file"
          id="audio-upload"
          accept="audio/*"
          onChange={handleFileSelect}
          className="hidden"
          disabled={isProcessing}
        />
        <Button variant="outline" size="sm" onClick={() => document.getElementById('audio-upload')?.click()} disabled={isProcessing}>
          Choose File
        </Button>
      </div>

      {selectedFile && (
        <Card style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }} className="p-3">
          <p className="text-xs font-medium mb-2" style={{ color: 'var(--text-primary)' }}>
            {selectedFile.name}
          </p>
          <p className="text-[10px] mb-3" style={{ color: 'var(--text-secondary)' }}>
            {(selectedFile.size / 1024 / 1024).toFixed(2)} MB
          </p>
          <Button
            onClick={handleTranscribe}
            disabled={isProcessing}
            className="w-full"
            style={{
              background: 'var(--accent)',
              color: 'var(--bg)',
            }}
          >
            {isProcessing ? 'Transcribing...' : 'Transcribe'}
          </Button>
        </Card>
      )}
    </div>
  )
}

function HistoryTab({ items, loading }: { items: GeneratedAudio[]; loading: boolean }) {
  if (loading) {
    return (
      <div className="space-y-2">
        {[1, 2, 3].map(i => (
          <div key={i} className="h-16 rounded" style={{ background: 'var(--mic-bg)' }} />
        ))}
      </div>
    )
  }

  if (items.length === 0) {
    return (
      <div className="text-center py-8">
        <p className="text-sm" style={{ color: 'var(--text-secondary)' }}>
          No generations yet. Create your first one!
        </p>
      </div>
    )
  }

  return (
    <div className="space-y-2">
      {items.map(item => (
        <Card
          key={item.id}
          style={{ background: 'var(--mic-bg)', borderColor: 'var(--pill-border)' }}
          className="p-3"
        >
          <div className="flex items-start justify-between gap-2 mb-2">
            <div className="min-w-0 flex-1">
              <p className="text-xs font-medium truncate" style={{ color: 'var(--text-primary)' }}>
                {item.voice_name}
              </p>
              <p className="text-[10px]" style={{ color: 'var(--text-secondary)' }}>
                {formatDate(item.created_at)}
              </p>
            </div>
            {item.duration && (
              <span className="text-[10px] px-2 py-1 rounded" style={{
                background: 'color-mix(in srgb, var(--accent) 10%, transparent)',
                color: 'var(--text-secondary)',
              }}>
                {item.duration.toFixed(1)}s
              </span>
            )}
          </div>
          <p className="text-xs line-clamp-2 mb-2" style={{ color: 'var(--text-secondary)' }}>
            {item.text}
          </p>
          {item.audio_url && (
            <audio
              controls
              src={item.audio_url}
              className="w-full h-6"
              style={{
                filter: 'invert(1)',
              }}
            />
          )}
        </Card>
      ))}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Main View
// ---------------------------------------------------------------------------

export default function ElevenlabsView(_props: PluginViewProps) {
  const [activeTab, setActiveTab] = useState<TabView>('synthesize')
  const [voices, setVoices] = useState<Voice[]>([])
  const [quota, setQuota] = useState<QuotaInfo>({
    character_limit: 100000,
    character_count: 0,
    remaining: 100000,
  })
  const [history, setHistory] = useState<GeneratedAudio[]>([])
  const [selectedVoice, setSelectedVoice] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [isGenerating, setIsGenerating] = useState(false)

  const loadData = useCallback(async () => {
    try {
      setLoading(true)
      setError(null)

      const [statusRes, itemsRes] = await Promise.allSettled([
        queryPluginStatus(PLUGIN_NAME),
        queryPluginItems(PLUGIN_NAME),
      ])

      if (statusRes.status === 'fulfilled') {
        const status = statusRes.value as unknown as ElevenlabsStatus
        setQuota(status.quota)
        // Mock voices - in real app, would come from API
        const mockVoices: Voice[] = [
          { id: 'voice-1', name: 'English Male', category: 'male', accent: 'British' },
          { id: 'voice-2', name: 'English Female', category: 'female', accent: 'American' },
          { id: 'voice-3', name: 'English Neutral', category: 'neutral', accent: 'Standard' },
          { id: 'voice-4', name: 'English Deep', category: 'male', accent: 'Deep' },
          { id: 'voice-5', name: 'English Bright', category: 'female', accent: 'Bright' },
          { id: 'voice-6', name: 'English Warm', category: 'female', accent: 'Warm' },
        ]
        setVoices(mockVoices)
        if (mockVoices.length > 0) {
          setSelectedVoice(mockVoices[0].id)
        }
      }

      if (itemsRes.status === 'fulfilled') {
        const items = itemsRes.value as unknown as GeneratedAudio[]
        setHistory(Array.isArray(items) ? items : [])
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load elevenlabs data')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    loadData()
    const id = setInterval(loadData, 30_000)
    return () => clearInterval(id)
  }, [loadData])

  const handleGenerate = useCallback(
    async (text: string, voiceId: string, speed: number, format: AudioFormat) => {
      setIsGenerating(true)
      try {
        await pluginAction(PLUGIN_NAME, 'generate', JSON.stringify({ text, voice_id: voiceId, speed, format }))
        await loadData()
      } catch (err) {
        setError(err instanceof Error ? err.message : 'Failed to generate audio')
      } finally {
        setIsGenerating(false)
      }
    },
    [loadData]
  )

  const handleTranscribe = useCallback(
    async (file: File) => {
      setIsGenerating(true)
      try {
        // In a real app, you'd upload the file
        await pluginAction(PLUGIN_NAME, 'transcribe', file.name)
        await loadData()
      } catch (err) {
        setError(err instanceof Error ? err.message : 'Failed to transcribe audio')
      } finally {
        setIsGenerating(false)
      }
    },
    [loadData]
  )

  if (loading && voices.length === 0) {
    return (
      <div className="flex flex-col gap-4 p-4 animate-pulse">
        <div className="grid grid-cols-4 gap-2">
          {[1, 2, 3, 4].map(i => (
            <div key={i} className="h-12 rounded" style={{ background: 'var(--mic-bg)' }} />
          ))}
        </div>
        <div className="h-32 rounded" style={{ background: 'var(--mic-bg)' }} />
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-4 p-4">
      {/* Error banner */}
      {error && (
        <div
          className="rounded px-3 py-2 text-xs"
          style={{
            background: 'color-mix(in srgb, var(--color-error) 12%, transparent)',
            color: 'var(--color-error)',
          }}
        >
          {error}
          <Button
            size="sm"
            variant="outline"
            className="ml-2 h-5 text-[10px] px-1.5"
            onClick={() => setError(null)}
          >
            Dismiss
          </Button>
        </div>
      )}

      {/* Tabs */}
      <nav className="flex gap-1 border-b" style={{ borderColor: 'var(--pill-border)' }} role="tablist">
        <TabButton
          label="Synthesize"
          isActive={activeTab === 'synthesize'}
          onClick={() => setActiveTab('synthesize')}
        />
        <TabButton
          label="Transcribe"
          isActive={activeTab === 'transcribe'}
          onClick={() => setActiveTab('transcribe')}
        />
        <TabButton
          label="History"
          isActive={activeTab === 'history'}
          onClick={() => setActiveTab('history')}
        />
      </nav>

      {/* Tab Content */}
      <div>
        {activeTab === 'synthesize' && (
          <SynthesizeTab
            voices={voices}
            quota={quota}
            selectedVoice={selectedVoice}
            onVoiceSelect={setSelectedVoice}
            onGenerate={handleGenerate}
            isGenerating={isGenerating}
          />
        )}

        {activeTab === 'transcribe' && (
          <TranscribeTab onTranscribe={handleTranscribe} isProcessing={isGenerating} />
        )}

        {activeTab === 'history' && (
          <HistoryTab items={history} loading={loading} />
        )}
      </div>
    </div>
  )
}
