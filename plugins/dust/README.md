# Dust

Dust is the experimental Tauri/React desktop and plugin-protocol source in this
public repository. It has grown beyond the original shell prototype: the tree
contains a Rust plugin host/registry, SDK and conformance fixtures, React scenes,
and Tauri commands for missions, files, Git/diffs, notifications, and commit
summaries. Some UI paths depend on configured local CLI services.

This is not the retired Wails `plugins/dashboard` application and is not the Rust
orchestrator rewrite. The root installer does not select Dust.

## Source layout

- `dust-core/`, `dust-sdk/`, `dust-registry/`: protocol and host crates.
- `dust-conformance/`: protocol fixtures and checks.
- `dust-dashboard/`: dashboard crate in the Cargo workspace.
- `src/`: React/Vite frontend.
- `src-tauri/`: Tauri application and command implementations; excluded from the
  root Rust workspace and built through Tauri.

## Development commands

The npm scripts declare these entrypoints:

```bash
cd plugins/dust
npm install
npm run dev
npm test
npm run build
npm run tauri:dev
```

`npm run dev` starts the frontend only. `tauri:dev` starts the desktop app and
requires Rust/Cargo and Tauri's platform prerequisites in addition to Node/npm.
`npm install` installs dependencies and can run package lifecycle scripts.
These commands document the shipped scripts; this documentation update did not
verify a complete desktop build or live plugin session.

For protocol details, see the [wire specification](../../docs/DUST-WIRE-SPEC.md)
and [known gaps](../../docs/DUST-WIRE-SPEC-GAPS.md). Check the current manifests
before selecting a crate or feature; do not infer implementation completeness
from the presence of a scene or command.
