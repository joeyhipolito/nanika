import { useMemo } from 'react'
import { useFiles } from '../hooks/useFiles'
import { FilesPanel } from './FilesPanel'
import { FileViewer, type FileViewerLanguage } from './FileViewer'
import type { FileEntry as MockFileEntry } from '../mocks/files'
import type { FileEntry } from '../types'

// LiveFilesPanel — wires the M4 live filesystem surface to the existing
// FilesPanel + FileViewer presentational components. The dust Rust commands
// return absolute paths and `kind: 'file' | 'dir'`; FilesPanel expects the
// mock shape (`'folder' | 'file'` plus a derived `ext`). We adapt at this
// boundary so the 48 existing scenes that pass MOCK_FILE_TREE keep working
// without any prop signature changes.
//
// Repo root is hard-coded for the M4 demo; M7 wires the project picker.

const DEMO_REPO_ROOT = '/Users/joeyhipolito/nanika'

const LANGUAGE_BY_EXT: Record<string, FileViewerLanguage> = {
  ts:   'ts',
  tsx:  'tsx',
  md:   'md',
  json: 'json',
  sh:   'sh',
}

function languageFor(filename: string): FileViewerLanguage {
  const ext = filename.split('.').pop()?.toLowerCase() ?? ''
  return LANGUAGE_BY_EXT[ext] ?? 'plain'
}

function relativize(path: string, root: string): string {
  if (!path.startsWith(root)) return path
  const rel = path.slice(root.length)
  return rel.startsWith('/') ? rel.slice(1) : rel
}

function toMockShape(entry: FileEntry, root: string): MockFileEntry {
  const path = relativize(entry.path, root)
  if (entry.kind === 'dir') {
    return { kind: 'folder', name: entry.name, path }
  }
  const ext = entry.name.split('.').pop() ?? ''
  return { kind: 'file', name: entry.name, path, ext }
}

export interface LiveFilesPanelProps {
  repoRoot?: string
}

export function LiveFilesPanel({ repoRoot = DEMO_REPO_ROOT }: LiveFilesPanelProps) {
  const files = useFiles(repoRoot)

  // Map absolute → relative paths for display, and Rust kind ('dir') to the
  // mock kind ('folder') the existing FilesPanel renders.
  const adapted = useMemo<MockFileEntry[]>(
    () => files.tree.map(e => toMockShape(e, repoRoot)),
    [files.tree, repoRoot],
  )

  // Resolve the relative path the user clicked back to an absolute path
  // before invoking openFile (the Rust read_file command is HOME-scoped).
  function handleSelect(relPath: string) {
    const abs = relPath.startsWith('/') ? relPath : `${repoRoot}/${relPath}`
    files.openFile(abs)
  }

  const viewerLines  = files.viewer.content?.split('\n') ?? []
  const viewerOpen   = files.viewer.content !== null || files.viewer.loading || files.viewer.error !== null
  const viewerName   = files.viewer.error ?? (files.viewer.loading ? 'loading…' : 'live file')

  return (
    <div style={{ display: 'flex', gap: '16px', alignItems: 'flex-start' }}>
      <FilesPanel
        files={adapted}
        onSelect={handleSelect}
      />
      {viewerOpen && (
        <FileViewer
          filename={viewerName}
          language={languageFor(viewerName)}
          lines={viewerLines}
        />
      )}
    </div>
  )
}
