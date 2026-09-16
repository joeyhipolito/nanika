//! Shared harness for the B5-DESIGN §4 gates in this crate.
//!
//! Included with `mod support;`, the same way `orchestrator-app`'s gates share
//! `tests/support/mod.rs`. It exists so the oracle contract has exactly one
//! definition: four gate binaries depend on it, and a rule about what counts as
//! an acceptable oracle should not be able to drift between them.

use std::path::{Path, PathBuf};

/// The frozen in-tree Go orchestrator every gate in this crate is compared
/// against.
///
/// B5-DESIGN §3.1: the oracle is in-tree and **mandatory**, and
/// `ORCHESTRATOR_ACCEPTED_GO_BIN` is a *cache* of that build rather than an
/// alternative source of truth — which is why the fallback below is the
/// workspace's own `target/go-oracle/orchestrator` and not the installed
/// `~/.alluka/bin/orchestrator` that the older `accepted_go_binary` helpers
/// use. An installed binary was once built from uncommitted `run.go`
/// (TRK-1280), so it cannot witness a claim about a committed tree.
///
/// This must never resolve to a silent skip: a missing oracle is an `Err` that
/// names exactly what was checked and where, and every caller propagates it
/// with `?`, so the gate goes red rather than green.
///
/// B5-DESIGN §8.6 precedence, first match wins:
///
/// 1. **The lease.** When `NANIKA_GO_ORACLE_OUTPUT_DIR` is set the oracle is
///    `$NANIKA_GO_ORACLE_OUTPUT_DIR/orchestrator` and nothing else is
///    consulted — `ORCHESTRATOR_ACCEPTED_GO_BIN` is not read even if somehow
///    present. Inside the lease there is one oracle and the lease named it:
///    built from the tested commit's `git archive`, against a `go.sum`-verified
///    module snapshot, by a GOROOT-bound toolchain whose digest is recorded.
/// 2. **`ORCHESTRATOR_ACCEPTED_GO_BIN`, bare runs only.** Unchanged in
///    meaning, and unreachable inside the lease by construction: the
///    gatekeeper's three-variable passthrough excludes it.
/// 3. **The in-tree build.**
pub fn frozen_go_oracle() -> Result<PathBuf, String> {
    let manifest = frozen_tree_manifest();
    if !manifest.is_file() {
        return Err(format!(
            "the frozen Go oracle manifest {} is missing; the in-tree oracle cannot be identified",
            manifest.display()
        ));
    }
    if let Some(directory) = std::env::var_os("NANIKA_GO_ORACLE_OUTPUT_DIR") {
        let leased = PathBuf::from(directory).join("orchestrator");
        return if leased.is_file() {
            Ok(leased)
        } else {
            Err(format!(
                "NANIKA_GO_ORACLE_OUTPUT_DIR names no built Go oracle at {}; the verification \
                 lease did not produce one (manifest: {})",
                leased.display(),
                manifest.display()
            ))
        };
    }
    if let Some(path) = std::env::var_os("ORCHESTRATOR_ACCEPTED_GO_BIN") {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "ORCHESTRATOR_ACCEPTED_GO_BIN={} does not name a file; the accepted Go oracle binary is missing (manifest: {})",
                path.display(),
                manifest.display()
            ))
        };
    }
    let built = workspace_root().join("target/go-oracle/orchestrator");
    if built.is_file() {
        Ok(built)
    } else {
        Err(format!(
            "the frozen Go oracle binary is missing: neither ORCHESTRATOR_ACCEPTED_GO_BIN nor the in-tree build {} names an existing file (manifest: {})",
            built.display(),
            manifest.display()
        ))
    }
}

/// The manifest that identifies the frozen Go tree the oracle is built from.
pub fn frozen_tree_manifest() -> PathBuf {
    workspace_root().join("tests/go-oracle/frozen-tree-manifest.json")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .to_path_buf()
}
