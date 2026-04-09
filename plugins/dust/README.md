# Dust

`dust` is the Rust replacement shell for the current dashboard. This first cut is intentionally isolated from Nanika internals and only chases the desktop behavior:

- `Option+Space` toggles the window
- frameless transparent palette-style window
- hide on blur / click outside
- small Raycast-like command surface for UI tuning

## Run

```bash
cd plugins/dust
npm install
npm run tauri:dev
```

## Current Scope

- Rust host: Tauri
- Frontend: React + Vite
- No plugin protocol, no dashboard channel, no Nanika data plumbing yet

The point of this stage is to get the shell feel right before wiring the rest of the system back in.
