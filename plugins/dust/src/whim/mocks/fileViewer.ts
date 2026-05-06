// Mock file source for FileViewer

export interface FileViewerFixture {
  filename: string
  language: 'ts' | 'tsx' | 'md' | 'json' | 'sh' | 'plain'
  lines:    string[]
}

export const MOCK_FILE_VIEWER: FileViewerFixture = {
  filename: 'src/components/PaletteShell.tsx',
  language: 'tsx',
  lines: [
    "// PaletteShell — composes pill + palette + canvas surfaces",
    "import { useState, useEffect } from 'react'",
    "import type { ReactNode } from 'react'",
    "",
    "export type ShellScale = 'pill' | 'palette' | 'detail' | 'canvas'",
    "export type PillMode  = 'idle' | 'hover' | 'type' | 'voice'",
    "",
    "interface PaletteShellProps {",
    "  scale:    ShellScale",
    "  pillMode: PillMode",
    "  children: ReactNode",
    "}",
    "",
    "// Width and height tokens for each scale stop",
    "const SCALE_WIDTH: Record<ShellScale, number> = {",
    "  pill:    320,",
    "  palette: 720,",
    "  detail:  960,",
    "  canvas:  1440,",
    "}",
    "",
    "export function PaletteShell({ scale, pillMode, children }: PaletteShellProps) {",
    "  const [mounted, setMounted] = useState(false)",
    "  useEffect(() => { setMounted(true) }, [])",
    "",
    "  const width = SCALE_WIDTH[scale]",
    "  return (",
    "    <div style={{ width, opacity: mounted ? 1 : 0 }}>",
    "      {children}",
    "    </div>",
    "  )",
    "}",
    "",
    "/* Local listener: focus management for pill modes. */",
    "export function usePillFocus(mode: PillMode) {",
    "  return mode === 'type' || mode === 'voice'",
    "}",
  ],
}
