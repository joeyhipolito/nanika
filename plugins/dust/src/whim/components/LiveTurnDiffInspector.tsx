import { useDiff } from '../hooks/useDiff'
import { TurnDiffInspector } from './TurnDiffInspector'

const DEMO_REPO_ROOT = '/Users/joeyhipolito/nanika'

export interface LiveTurnDiffInspectorProps {
  repoRoot?: string
}

export function LiveTurnDiffInspector({ repoRoot = DEMO_REPO_ROOT }: LiveTurnDiffInspectorProps) {
  const { changedFiles, activePath, setActivePath, hunks, acceptHunk, rejectHunk } =
    useDiff(repoRoot)

  return (
    <TurnDiffInspector
      files={changedFiles}
      activePath={activePath}
      onSetActivePath={setActivePath}
      hunks={hunks}
      acceptHunk={acceptHunk}
      rejectHunk={rejectHunk}
    />
  )
}
