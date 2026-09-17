# Matched standalone Codex experiments

`orchestrator-experiment` runs repeated OFF/ON pairs against the explicitly supplied Rust pilot. Pair order alternates OFF/ON, then ON/OFF. Every sample gets its own pilot output and disposable workspace. The source repository must remain clean and unchanged. The runner does not install providers or promote workspace edits.

Build the runner and its process broker in release mode, keeping both executables together:

```sh
cargo build --release -p orchestrator-first-use-pilot --bin orchestrator-experiment -p orchestrator-process --bin orchestrator-process-broker
```

The pilot executable must also have its matching `orchestrator-output` and `orchestrator-process-broker` siblings. Use canonical absolute paths to owned executables without group/other write permission. File arguments cannot be symlinks. The output parent must already exist; the output itself must be new and outside the source repository.

```sh
orchestrator-experiment --allow-provider-experiments \
  --pilot /absolute/bundle/orchestrator-first-use-pilot \
  --provider /absolute/release/codex \
  --repo /absolute/clean-task-repository \
  --prompt-file /absolute/task.md \
  --output-dir /absolute/new-experiment \
  --model gpt-5.6-luna --persona general-purpose --pairs 2 \
  --timeout-secs 120 --verification-timeout-secs 30 \
  -- /absolute/verify-task
```

The final argv is passed literally, without a shell. The same verifier runs in every available workspace, including failed provider attempts. Only the verifier executable is hashed: if it is an interpreter, script arguments and other external dependencies remain caller-controlled. Use an executable frozen verifier script or inline checks and preserve their dependencies for a matched quality gate. A successful provider response alone cannot pass verification.

The runner accepts 2–10 pairs. Per-process timeouts are bounded at 1–1800 seconds; pilot supervision allows an extra 30 seconds for its cleanup. Process output and each imported JSON artifact are capped at 1 MiB. Prompts are capped at 1 MiB; pinned files at 512 MiB. All child processes, including Git probes and verification, use the native process supervisor. Release mode avoids expensive debug hashing when repeatedly validating large provider executables.

`manifest.json` records source commit/tree, frozen prompt, executable SHA256 values, provider version, pilot catalog, model/persona and verifier argv. The pilot has no version command, so its version remains unknown and its SHA256/catalog identify the build. The report preserves route-derived effort and authoritative usage fields from each pilot result.

`report.json` is replaced after every completed sample. All planned samples remain present. A later source or pinned-input change excludes the current sample and stops further dispatch with explicit reasons. Each sample records pilot and verifier outcomes, durations, full available result/feature/usage/Portal receipts, failed command observations and individual command exits. ON requires observed Portal application and at least one verified wrapped call. Missing usage remains null; it does not fabricate zero tokens or cost. Report generation can succeed even when every quality gate fails; inspect `quality_passed`, not the runner's exit code, for task quality. Admission or report-writing failures return a nonzero runner exit.

Arm summaries describe all observed sample durations, including failed quality gates, and keep unknown duration totals null. Pair numbers and per-sample metrics preserve the matched ordering. Sample elapsed time includes the pilot and verifier; source/pin checks between samples are outside that duration. Interrupted runs retain the last saved report; this initial implementation has no resume command or custom interruption report.

Cache state is **external/uncontrolled**. Provider-reported cached tokens do not establish cold or warm conditions. OFF provider capture may already truncate command output, so its full emitted command bytes remain unknown. ON full-log bytes are reported separately from retained provider capture. Every report has `savings_claim: false` and descriptive-only conclusions. Repeated samples make the evidence auditable; they do not by themselves establish a performance or cost improvement.

Only the supported standalone `code --runtime codex` Portal cap path is exercised. Review, fixed/durable missions and other feature activation are outside this runner's scope.
