import { useState } from 'react'
import type { RecentItem, Routine } from '../mocks/leftrail'
import { Icon } from '../icons/Icon'
import type { IconName } from '../icons/registry'

// ─── Types ────────────────────────────────────────────────────────────────────

type ModeTab = 'chat' | 'todo' | 'code'

interface LeftRailProps {
  recents:  RecentItem[]
  routines: Routine[]
  onSessionNew?:   () => void
  onRecentSelect?: (id: string) => void
}

// ─── LeftRail ─────────────────────────────────────────────────────────────────

export function LeftRail({ recents, routines, onSessionNew, onRecentSelect }: LeftRailProps) {
  const [activeTab, setActiveTab]             = useState<ModeTab>('chat')
  const [showRoutines, setShowRoutines]       = useState(false)
  const [showMore, setShowMore]               = useState(false)

  return (
    <div style={{
      width:         '240px',
      background:    'var(--s1)',
      borderRight:   '1px solid var(--border)',
      display:       'flex',
      flexDirection: 'column',
      fontFamily:    'var(--sans)',
      overflow:      'hidden',
      flexShrink:    0,
    }}>
      {/* Mode tabs */}
      <div style={{
        display:      'flex',
        borderBottom: '1px solid var(--border)',
        padding:      '6px 8px 0',
        gap:          '2px',
      }}>
        <ModeTabButton tab="chat" activeTab={activeTab} onSelect={setActiveTab} label="Chat" />
        <ModeTabButton tab="todo" activeTab={activeTab} onSelect={setActiveTab} label="Todo" />
        <ModeTabButton tab="code" activeTab={activeTab} onSelect={setActiveTab} label="Code" />
      </div>

      {/* Tab body */}
      <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflowY: 'auto' }}>
        {activeTab === 'chat' && (
          <ChatBody
            recents={recents}
            routines={routines}
            showRoutines={showRoutines}
            showMore={showMore}
            onSessionNew={onSessionNew}
            onRecentSelect={onRecentSelect}
            onToggleRoutines={() => setShowRoutines(s => !s)}
            onToggleMore={() => setShowMore(s => !s)}
          />
        )}
        {activeTab === 'todo' && <PlaceholderBody label="Todo list" />}
        {activeTab === 'code' && <PlaceholderBody label="Code sessions" />}
      </div>
    </div>
  )
}

// ─── Mode tab button ──────────────────────────────────────────────────────────

const TAB_ICON: Record<ModeTab, IconName> = {
  chat: 'ModeChat',
  todo: 'ModeTodo',
  code: 'ModeCode',
}

function ModeTabButton({
  tab,
  activeTab,
  onSelect,
  label,
}: {
  tab: ModeTab
  activeTab: ModeTab
  onSelect: (t: ModeTab) => void
  label: string
}) {
  const isActive = tab === activeTab
  return (
    <button
      onClick={() => onSelect(tab)}
      aria-label={label}
      aria-pressed={isActive}
      style={{
        display:       'flex',
        alignItems:    'center',
        justifyContent: 'center',
        width:         '36px',
        height:        '32px',
        borderRadius:  '6px 6px 0 0',
        border:        'none',
        cursor:        'pointer',
        background:    isActive ? 'var(--s2)' : 'transparent',
        color:         isActive ? 'var(--accent)' : 'var(--faint)',
        borderBottom:  isActive ? '2px solid var(--accent)' : '2px solid transparent',
        outline:       isActive ? '1px solid var(--accent-rim)' : 'none',
        outlineOffset: '-1px',
        transition:    'background 120ms, color 120ms',
      }}
    >
      <Icon name={TAB_ICON[tab]} variant={isActive ? 'duotone' : 'line'} size={15} />
    </button>
  )
}

// ─── Chat body ────────────────────────────────────────────────────────────────

function ChatBody({
  recents,
  routines,
  showRoutines,
  showMore,
  onSessionNew,
  onRecentSelect,
  onToggleRoutines,
  onToggleMore,
}: {
  recents: RecentItem[]
  routines: Routine[]
  showRoutines: boolean
  showMore: boolean
  onSessionNew?: () => void
  onRecentSelect?: (id: string) => void
  onToggleRoutines: () => void
  onToggleMore: () => void
}) {
  return (
    <>
      {/* Action rows */}
      <div style={{ padding: '8px 0 4px' }}>
        <ActionRow onClick={onSessionNew} primary>
          <Icon name="Plus" size={14} /> New session
        </ActionRow>
        <ActionRow onClick={onToggleRoutines}>
          <Icon name="BoltDefault" size={14} />
          Routines{routines.length > 0 && <span style={{ marginLeft: 'auto', color: 'var(--faint)', fontSize: '10px' }}>{routines.length}</span>}
        </ActionRow>
        {showRoutines && (
          <div style={{ paddingLeft: '12px', borderLeft: '1px solid var(--border-soft)', marginLeft: '16px' }}>
            {routines.map(r => (
              <div key={r.id} style={{
                padding:   '5px 12px',
                fontSize:  '12px',
                color:     'var(--muted)',
                display:   'flex',
                gap:       '6px',
                alignItems: 'baseline',
              }}>
                <span style={{ flex: 1 }}>{r.label}</span>
                <span style={{ fontSize: '10px', color: 'var(--faint)', flexShrink: 0 }}>{r.schedule}</span>
              </div>
            ))}
          </div>
        )}
        <ActionRow>
          <Icon name="Customize" size={14} /> Customize
        </ActionRow>
        <ActionRow onClick={onToggleMore}>
          <Icon name="MoreVertical" size={14} /> More
        </ActionRow>
        {showMore && (
          <div style={{ paddingLeft: '24px' }}>
            {['Extensions', 'Settings', 'Help'].map(item => (
              <ActionRow key={item}>{item}</ActionRow>
            ))}
          </div>
        )}
      </div>

      <Divider />

      {/* Pinned */}
      <SectionHeader><Icon name="Pin" size={9} style={{ verticalAlign: 'middle', marginRight: '3px' }} />Pinned</SectionHeader>
      <div style={{
        margin:      '4px 12px 8px',
        padding:     '8px 12px',
        background:  'var(--s2)',
        borderRadius: '6px',
        fontSize:    '11.5px',
        color:       'var(--ghost)',
        textAlign:   'center',
        border:      '1px dashed var(--border)',
      }}>
        Drag to pin
      </div>

      <Divider />

      {/* Recents */}
      <SectionHeader>Recents</SectionHeader>
      <div style={{ paddingBottom: '8px' }}>
        {recents.map(item => (
          <button
            key={item.id}
            onClick={() => onRecentSelect?.(item.id)}
            style={{
              display:    'flex',
              flexDirection: 'column',
              width:      '100%',
              background: 'transparent',
              border:     'none',
              padding:    '5px 14px',
              cursor:     'pointer',
              textAlign:  'left',
              gap:        '2px',
            }}
          >
            <span style={{ fontSize: '12.5px', color: 'var(--text)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', display: 'block' }}>
              {item.title}
            </span>
            <div style={{ display: 'flex', gap: '6px', alignItems: 'center' }}>
              <span style={{ fontSize: '10.5px', color: 'var(--faint)' }}>{item.projectName}</span>
              <span style={{ fontSize: '10px', color: 'var(--ghost)' }}>·</span>
              <span style={{ fontSize: '10.5px', color: 'var(--faint)' }}>{item.agoLabel}</span>
            </div>
          </button>
        ))}
      </div>
    </>
  )
}

// ─── Placeholder body ─────────────────────────────────────────────────────────

function PlaceholderBody({ label }: { label: string }) {
  return (
    <div style={{
      flex:          1,
      display:       'flex',
      alignItems:    'center',
      justifyContent: 'center',
      color:         'var(--ghost)',
      fontSize:      '12px',
      fontFamily:    'var(--mono)',
      padding:       '24px',
      textAlign:     'center',
    }}>
      {label}
    </div>
  )
}

// ─── Shared sub-components ────────────────────────────────────────────────────

function ActionRow({
  children,
  onClick,
  primary,
}: {
  children: React.ReactNode
  onClick?: () => void
  primary?: boolean
}) {
  return (
    <button
      onClick={onClick}
      style={{
        display:    'flex',
        alignItems: 'center',
        width:      '100%',
        background: 'transparent',
        border:     'none',
        padding:    '6px 14px',
        cursor:     'pointer',
        fontSize:   '13px',
        color:      primary ? 'var(--accent)' : 'var(--muted)',
        fontFamily: 'var(--sans)',
        textAlign:  'left',
        gap:        '4px',
      }}
    >
      {children}
    </button>
  )
}

function SectionHeader({ children }: { children: React.ReactNode }) {
  return (
    <h6 style={{
      margin:        '4px 0 2px',
      padding:       '0 14px',
      fontFamily:    'var(--mono)',
      fontSize:      '9.5px',
      fontWeight:    600,
      letterSpacing: '0.10em',
      color:         'var(--ghost)',
      textTransform: 'uppercase',
    }}>
      {children}
    </h6>
  )
}

function Divider() {
  return <div style={{ height: '1px', background: 'var(--border-soft)', margin: '4px 0' }} />
}

