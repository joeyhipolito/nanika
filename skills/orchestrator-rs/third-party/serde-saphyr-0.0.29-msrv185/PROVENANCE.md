# serde-saphyr 0.0.29 Rust 1.85 compatibility patch

The upstream crate archive is preserved at
`upstream/serde-saphyr-0.0.29.crate`. Its SHA-256 is the checksum recorded for
`serde-saphyr 0.0.29` in the original Cargo lockfile:

`7bd22781911de0ca6debda95f073c8f18bec65d1a94f1fa9573f3102e514cea4`

`source/` is an extraction of that archive with only syntax-level MSRV edits:
let-chains are expressed as equivalent nested conditions and unstable integer
`is_multiple_of` calls are expressed with remainder checks. Parser limits,
features, APIs, error values, dependencies, and defaults are unchanged.

Run `go run ./third-party/serde-saphyr-0.0.29-msrv185/tools/generate-manifest/main.go`
from the Rust workspace to regenerate `patch-manifest.json`. The provenance
test independently verifies the archive, complete original file manifest, and
every before/after delta.
