# Offline experiment ledger

Open `index.html` directly in a browser and choose the Rust experiment runner's `report.json`. No server, dependency install, network connection or provider process is required. Keep the HTML, CSS and three JavaScript files together.

The viewer supports bounded `nanika.portal-experiment.v1` reports: at most 4 MiB, 2–10 matched pairs, the standalone Codex Portal cap catalog, known counters and immutable feature receipt schemas v1/v2. It rejects inconsistent sample order, quality claims, unknown top-level schema fields, duplicate features and invalid counters. Missing usage remains unknown. It checks the report's structure and internal consistency; it does not re-hash local executables or authenticate the report's author.

Arm and quality filters affect the comparison table. All attempted samples remain in the receipt section. Expanded receipts separate requested/effective settings from observed application, list quality gates, command failures, usage and duration observations. OFF full command bytes remain unknown. The page never renders raw prompts or tool output. All imported text is assigned as text, never HTML, and a restrictive content policy disallows network requests.

This is the read-only comparison slice of TRK-1498. Starting a run, changing feature settings, review/durable ON modes, and any additional feature integrations remain unavailable. The disabled mode control describes the current supported scope; it is not an execution control.

Native Rust orchestration with Claude Sonnet authored `feature-rows.js` and its tests. The file-import, validation, DOM adapter and styling were integrated directly and independently reviewed. The interface extends the existing Nanika sumi/washi visual language.

Run the deterministic checks with Node:

```sh
node feature-rows.test.cjs
node report-schema.test.cjs /absolute/path/to/report.json
```

The second check intentionally consumes a real completed runner report containing both a passing ON sample and an earlier failed ON sample. Browser acceptance additionally covers local file import, filters, unsupported controls, invalid/oversized files, inert markup, and desktop/mobile rendering.
