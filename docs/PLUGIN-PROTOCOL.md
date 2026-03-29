---
produced_by: technical-writer
phase: phase-2
workspace: 20260329-0ec406b5
created_at: "2026-03-29T20:15:00Z"
confidence: high
depends_on: []
token_estimate: 4200
---

# Plugin Protocol

Nanika plugins extend the dashboard with custom capabilities: status monitoring, item listings, and user actions. The plugin protocol defines how plugins declare themselves, expose queries, and render custom UI.

## Overview

The plugin system has three layers:

1. **Discovery** — Dashboard scans `~/nanika/plugins/*/plugin.json` at startup
2. **Query** — Dashboard invokes `<binary> query {status|items|action} --json` to fetch data
3. **Rendering** — Custom plugins provide `ui/dist/index.js` for dynamic loading via blob URL + `import()`

## Plugin Discovery

### File Layout

```
~/nanika/plugins/<name>/
├── plugin.json                    # Plugin manifest
├── bin/<binary>                   # Compiled binary (CLI)
└── ui/
    ├── index.tsx                  # React source (optional)
    ├── package.json
    ├── vite.config.ts
    └── dist/index.js              # Prebuilt bundle (optional)
```

### ListPlugins() Scan

The dashboard's `ListPlugins()` function (from `plugins/dashboard/app.go`) scans for plugins:

```go
func (a *App) ListPlugins() ([]PluginManifest, error)
  • Scans: ~/nanika/plugins/*/plugin.json
  • Filters: api_version >= 1
  • Returns: []PluginManifest with metadata for each plugin
```

A plugin is **not discovered** if:
- `plugin.json` is missing
- `api_version < 1`
- JSON is malformed

The dashboard can discover plugins at any time, so the `ListPlugins()` result is not cached — plugins installed or updated while the dashboard is running will appear on the next refresh.

## plugin.json Schema

### Required Fields

| Field | Type | Notes |
|-------|------|-------|
| `name` | string | Unique plugin identifier; used in CLI paths, IDs, and module names. Lowercase, no spaces. |
| `version` | string | SemVer (e.g. `1.0.0`). No functional use; for documentation. |
| `api_version` | int | Must be `1` for discovery. |

### Optional Fields

| Field | Type | Notes |
|-------|------|-------|
| `description` | string | One-liner shown in the dashboard UI. |
| `icon` | string | Icon key (e.g. `ListCheck`, `Calendar`). Maps to icon name via dashboard's icon registry. If missing or unmapped, defaults to generic plug icon. |
| `binary` | string | CLI binary name (e.g. `tracker`). Resolved via `$PATH` lookup. If missing, plugin is not queryable. |
| `build` | string | Build command (e.g. `cargo build --release`). For documentation only; not executed by the dashboard. |
| `install` | string | Install command. For documentation only. |
| `tags` | []string | Searchable keywords (e.g. `["issue-tracking", "task-management"]`). Shown in command palette. |
| `ui` | bool | If `true`, load custom UI bundle from `ui/dist/index.js`. Default: `false`. |
| `provides` | []string | Array of query types this plugin provides. Example: `["status", "items", "actions"]`. For documentation. |
| `actions` | object | Maps action keys to command templates or objects. See Query Protocol. |
| `repository` | object | Source metadata: `type` (git), `url`, `path`. For documentation. |

### Example: tracker

```json
{
  "name": "tracker",
  "version": "0.1.0",
  "api_version": 1,
  "description": "Local issue tracker with hierarchical relationships",
  "icon": "ListCheck",
  "binary": "tracker",
  "build": "cargo build --release",
  "install": "cp target/release/tracker ~/bin/tracker",
  "tags": ["issue-tracking", "task-management"],
  "ui": true,
  "provides": ["status", "items", "actions"],
  "actions": {
    "status": "tracker query status --json",
    "items": "tracker query items --json",
    "actions": "tracker query actions --json"
  },
  "repository": {
    "type": "git",
    "url": "https://github.com/joeyhipolito/nanika",
    "path": "plugins/tracker"
  }
}
```

### Example: scheduler

```json
{
  "name": "scheduler",
  "version": "1.0.0",
  "api_version": 1,
  "description": "Local job scheduler and social content publisher",
  "icon": "Calendar",
  "binary": "scheduler",
  "build": "go build -ldflags \"-s -w\" -o bin/scheduler ./cmd/scheduler-cli",
  "install": "ln -sf $(pwd)/bin/scheduler ~/bin/scheduler",
  "tags": ["scheduler", "cron", "jobs", "social"],
  "ui": true,
  "provides": ["query status", "query items", "query action"],
  "actions": {
    "status": {
      "cmd": ["scheduler", "query", "status", "--json"],
      "description": "Daemon running state, job count, next scheduled run time"
    },
    "items": {
      "cmd": ["scheduler", "query", "items", "--json"],
      "description": "List all jobs"
    },
    "action_run": {
      "cmd": ["scheduler", "query", "action", "run", "<job_id>", "--json"],
      "description": "Execute a job immediately"
    }
  }
}
```

## Query Protocol

### Overview

Dashboard calls `<binary> query <type> --json` and expects JSON output on stdout.

### Query Types

**status** — Overview and health of the plugin

```bash
<binary> query status --json
```

Return a JSON object (any shape) representing the plugin's overall status. Example:

```json
{
  "ok": true,
  "count": 42,
  "type": "tracker-status"
}
```

**items** — Itemized list for display in a table

```bash
<binary> query items --json
```

Return a JSON array of objects, where each object is a table row. Columns are inferred from the first item's keys.

```json
{
  "items": [
    { "id": "trk-1", "title": "Fix login bug", "status": "in-progress", "priority": "P0" },
    { "id": "trk-2", "title": "Add dark mode", "status": "open", "priority": "P1" }
  ],
  "count": 2
}
```

Or just an array:

```json
[
  { "id": "job-1", "name": "daily-backup", "last_run": "2026-03-29T08:00:00Z", "next_run": "2026-03-30T02:00:00Z" }
]
```

**actions** — List of available actions

```bash
<binary> query actions --json
```

Return a JSON array of action definitions:

```json
{
  "actions": [
    {
      "name": "next",
      "command": "tracker query action next",
      "description": "Show the highest-priority ready issue"
    }
  ]
}
```

**action &lt;verb&gt; [&lt;id&gt;]** — Execute a single action

```bash
<binary> query action run <job_id> --json
<binary> query action approve --json
```

Return a JSON object describing the result. Shape is plugin-defined, but should include `ok: boolean`:

```json
{
  "ok": true,
  "message": "Job executed successfully",
  "exit_code": 0
}
```

### JSON Envelope (Optional)

Plugins may wrap responses in an envelope for clarity, but the dashboard expects the actual data (array, object) to be parseable as JSON. No strict envelope format is enforced — the dashboard uses `json.Unmarshal(data, &target)` where `target` matches the expected shape (array for items, object for status).

### Action Command Templates

In `plugin.json`, actions can be:

1. **String** — Direct shell command:
   ```json
   "actions": {
     "status": "tracker query status --json"
   }
   ```

2. **Object** — Command with metadata:
   ```json
   "actions": {
     "status": {
       "cmd": ["tracker", "query", "status", "--json"],
       "description": "Current status"
     }
   }
   ```

3. **Per-item actions** — Contain ID placeholders detected by regex `/<[^>]+>/`:
   ```json
   "actions": {
     "action_run": {
       "cmd": ["scheduler", "query", "action", "run", "<job_id>", "--json"],
       "description": "Execute a job"
     }
   }
   ```

### Timeout

Dashboard queries time out after:
- **Status/items**: 15 seconds
- **Actions**: 30 seconds

If a query hangs or fails, the dashboard displays an error banner.

## Dashboard Microfrontend Contract

### Custom UI Bundle

Plugins with `ui: true` must provide a prebuilt bundle:

```
plugins/<name>/ui/dist/index.js
```

The bundle is:
- An ES module (not CommonJS, not IIFE)
- Prebuilt and minified (typically via Vite)
- Must export a React component as the **default export**

### Shared Modules

The dashboard initializes `window.__nanika_shared__` on App mount (before any plugins load). Plugins can import from shared modules to avoid bundling duplicates:

```typescript
interface NanikaShared {
  react: typeof React
  reactDom: typeof ReactDOM
  reactDomClient: typeof ReactDOMClient
  ui: {
    Button, buttonVariants,
    Badge, badgeVariants,
    Card, CardHeader, CardFooter, CardTitle, CardDescription, CardContent,
    Tabs, TabsList, TabsTrigger, TabsContent,
    cn  // classname utility
  }
  wails: {
    // Dashboard RPC methods exposed via Wails
    listPlugins, queryPluginStatus, queryPluginItems, pluginAction,
    listMissions, getMissionDetail, getMissionEvents, getMissionDAG,
    cancelMission, runMission,
    // ... more methods
  }
}
```

### Loading Flow

Dashboard's `usePlugins()` hook:

1. Calls `ListPlugins()`
2. For each plugin with `ui: true`:
   - Calls `GetPluginUIBundle(name)` → reads `~/nanika/plugins/<name>/ui/dist/index.js`
   - Creates a Blob URL from the JS source
   - Dynamic imports via `import(blobURL)` → extracts `default` export
   - Wraps in error boundary (so one plugin crash doesn't break the dashboard)
3. Registers the component in the module registry with ID `plugin:<name>`

### Component Contract

The default export must be a React functional component:

```typescript
interface PluginViewProps {
  isConnected?: boolean
}

export default function MyPluginUI({ isConnected }: PluginViewProps) {
  // ...
  return <div>...</div>
}
```

**Props:**
- `isConnected` — boolean indicating if the orchestrator is reachable (optional)

**Hooks available** (via shared modules):

- React hooks: `useState`, `useEffect`, `useCallback`, `useMemo`, `useRef`
- Dashboard components: `Button`, `Badge`, `Card`, `Tabs`
- Wails bridge: `queryPluginStatus()`, `pluginAction()`, etc.

### Vite Configuration

Plugin UIs are built with Vite. To use shared modules, add the `nanikaSharedPlugin()` to your Vite config:

```typescript
// vite.config.ts
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { nanikaSharedPlugin } from '@nanika/vite-plugin-shared'

export default defineConfig({
  plugins: [
    nanikaSharedPlugin(),  // Must come BEFORE @vitejs/plugin-react
    react(),
  ],
  build: {
    lib: {
      entry: 'index.tsx',
      name: 'plugin',
      fileName: 'index',
    },
  },
})
```

**Virtual modules** (automatically resolved to shared modules):

- `react` → `window.__nanika_shared__.react`
- `react-dom` → `window.__nanika_shared__.reactDom`
- `react-dom/client` → `window.__nanika_shared__.reactDomClient`
- `@nanika/ui` → `window.__nanika_shared__.ui`
- `@nanika/wails` → `window.__nanika_shared__.wails`

So you can write normal imports:

```typescript
import React, { useState } from 'react'
import { Button, Badge } from '@nanika/ui'
import { queryPluginItems } from '@nanika/wails'

export default function MyUI() {
  const [items, setItems] = useState([])
  // ...
}
```

### Fallback UI

If a plugin declares `ui: true` but the bundle load fails, the dashboard falls back to the generic `PluginModule` component, which:
- Calls `query status`, `query items`, `query actions` to fetch data
- Renders status as a grid, items as a table
- Provides buttons to trigger actions
- Shows the load error as a banner

This provides zero-effort UI for plugins that don't want custom rendering.

### Error Boundary

Each plugin UI is wrapped in a React error boundary. If rendering crashes, the dashboard:
- Logs the error to console
- Displays the generic fallback UI
- Dashboard remains functional

## Plugin Development Checklist

### 1. Create Manifest

```json
{
  "name": "myname",
  "version": "0.1.0",
  "api_version": 1,
  "description": "...",
  "binary": "myname",
  "build": "...",
  "tags": ["..."],
  "ui": false
}
```

### 2. Implement CLI Queries

```bash
# Build your binary to accept these commands:
myname query status --json   # Returns JSON object
myname query items --json    # Returns JSON array
myname query actions --json  # Returns { actions: [...] }
myname query action <verb> [<id>] --json  # Returns action result
```

Queries should be idempotent and complete within their timeouts.

### 3. (Optional) Build Custom UI

```bash
cd plugins/myname/ui
npm install
npm run build
# Builds to dist/index.js
```

Update `plugin.json`:

```json
{
  "ui": true
}
```

Component must export React component as default:

```typescript
// index.tsx
import { useState, useEffect } from 'react'
import { Button } from '@nanika/ui'
import { queryPluginStatus } from '@nanika/wails'

export default function MyUI({ isConnected }: { isConnected?: boolean }) {
  const [status, setStatus] = useState(null)
  useEffect(() => {
    queryPluginStatus('myname').then(setStatus)
  }, [])
  return <div>...</div>
}
```

### 4. Deploy

Symlink or copy binary to `~/bin/<name>`:

```bash
ln -s $(pwd)/bin/myname ~/bin/myname
```

Dashboard will auto-discover the plugin on next refresh.

## Binary Resolution

Dashboard resolves the plugin binary via `resolvePluginBinary()`:

1. Read `plugin.json` and extract `binary` field
2. Look up via `exec.LookPath(binary)` — checks `$PATH`
3. Fallback: Check `~/nanika/bin/<name>`

Env enrichment adds common user paths to `$PATH`:
- `~/bin`
- `~/.local/bin`
- `~/go/bin`
- `/opt/homebrew/bin`
- `/usr/local/bin`

So plugins installed in `~/bin` or via `go install` are automatically reachable.

## Patterns and Anti-Patterns

### ✅ DO

- **Use shared modules** — Import `react`, `@nanika/ui` normally; Vite plugin handles aliasing
- **Return clean JSON** — Status/items with consistent field names for better UI inference
- **Handle timeouts gracefully** — Queries should return quickly; cache heavy operations
- **Implement query status** — Even if just `{ "ok": true }` — shows plugin is registered
- **Use error boundaries** — Dashboard wraps your component, but good practice anyway
- **Make UI optional** — Not all plugins need custom rendering; fallback is solid

### ❌ DON'T

- **Bundle React/ReactDOM** — They're shared; bundling duplicates breaks the JSX runtime
- **Return invalid JSON** — Dashboard can't display partial or malformed responses
- **Assume $PATH** — Plugins might be invoked from a .app bundle; rely on `~/bin` or symlinks
- **Hardcode colors/fonts** — Use Tailwind utilities and CSS variables (`--color-success`, etc.)
- **Forget --json flag** — All query commands must output JSON, not human-readable text

## Learning Discoveries

**FINDING:** The Vite plugin must be listed before `@vitejs/plugin-react` in the plugins array. If listed after, React's plugin processes imports first, breaking the alias resolution.

**GOTCHA:** The jsx-runtime bundled in plugin UIs references `React.__SECRET_INTERNALS_DO_NOT_USE_OR_YOU_WILL_BE_FIRED`. This internal must be exported from the shared React module, or the bundle crashes on module init.

**PATTERN:** All plugins follow the same query protocol (status/items/actions) so the dashboard can provide a generic fallback UI. This makes UI optional but not required.

**LEARNING:** `window.__nanika_shared__` is initialized synchronously in App root (not in useEffect) because plugin loading in a child hook must see it already defined.

---

**Total lines:** 470 | **Confidence:** high | **Verified against:** PluginManifest, QueryPluginStatus, ListPlugins, GetPluginUIBundle, loadPluginBundle, shared-modules.ts, vite-plugin-nanika-shared.ts
