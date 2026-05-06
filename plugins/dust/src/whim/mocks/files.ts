// Mock file tree — drives FilesPanel

export type FileEntry =
  | { kind: 'folder'; name: string; path: string; expanded?: boolean }
  | { kind: 'file';   name: string; path: string; ext: string }

export const MOCK_FILE_TREE: FileEntry[] = [
  { kind: 'folder', name: 'src',            path: 'src',                        expanded: true },
  { kind: 'folder', name: 'components',     path: 'src/components',             expanded: true },
  { kind: 'file',   name: 'PaletteShell.tsx', path: 'src/components/PaletteShell.tsx', ext: 'tsx' },
  { kind: 'file',   name: 'LeftRail.tsx',   path: 'src/components/LeftRail.tsx',     ext: 'tsx' },
  { kind: 'file',   name: 'ProjectsTree.tsx', path: 'src/components/ProjectsTree.tsx', ext: 'tsx' },
  { kind: 'folder', name: 'mocks',          path: 'src/mocks',                  expanded: false },
  { kind: 'file',   name: 'diffs.ts',       path: 'src/mocks/diffs.ts',              ext: 'ts' },
  { kind: 'file',   name: 'events.ts',      path: 'src/mocks/events.ts',             ext: 'ts' },
  { kind: 'folder', name: 'scenarios',      path: 'src/scenarios',              expanded: false },
  { kind: 'file',   name: 'Scenes.tsx',     path: 'src/scenarios/Scenes.tsx',        ext: 'tsx' },
  { kind: 'folder', name: 'styles',         path: 'src/styles',                 expanded: false },
  { kind: 'file',   name: 'tokens.css',     path: 'src/styles/tokens.css',           ext: 'css' },
  { kind: 'file',   name: 'App.tsx',        path: 'src/App.tsx',                     ext: 'tsx' },
  { kind: 'file',   name: 'main.tsx',       path: 'src/main.tsx',                    ext: 'tsx' },
]
