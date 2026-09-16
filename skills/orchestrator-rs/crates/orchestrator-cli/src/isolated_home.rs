//! B5-DESIGN §7's opt-in isolated-home door, CLI half.
//!
//! `composition::seal` reaches this module when it was handed no fixture
//! enrollment — which is every invocation of the shipped binary. Everything
//! here is refusal-shaped: it either returns an [`IsolatedEnrollment`] that
//! satisfies every row of §7.3, or it returns `None` (D1 — the operator did not
//! ask) or an error naming the row that refused.
//!
//! Why the door lives here rather than in `composition.rs`: the composition
//! root reads the environment for seal 1's *paths* and for nothing else, and
//! `production_composition_e2e`'s N5 pins that by scanning `composition.rs` for
//! `env::var`. The environment reads a §7 enrollment needs are the manifest
//! cross-check below and `IsolatedHomeCanaryCapability::for_isolated_enrollment`
//! (which lives in `orchestrator-app`, beside the live-home selector it is
//! mutually exclusive with). The root itself still names no variable.

use std::{
    fs,
    path::{Path, PathBuf},
};

use orchestrator_app::{
    ApplicationError, AuthorizedIsolatedRuntimeHome, FreshFixtureAuthority,
    IsolatedHomeCanaryCapability, IsolatedHomeGuards, ResolvedRuntimeHome, attest_bundled_helper,
};
use orchestrator_core::MissionId;

/// The manifest's file name, beside the binaries in the installed bundle.
///
/// `scripts/build-production-bundle.sh` writes it (B5-DESIGN §7.2) and the
/// binary reads it from the directory holding `argv[0]`.
pub(crate) const BUNDLE_MANIFEST: &str = "bundle-manifest.txt";

/// The optional operator cross-check. It can never *supply* a digest; see
/// [`attest_bundled_helper`].
const HELPER_DIGEST_CROSS_CHECK: &str = "NANIKA_BUNDLED_HELPER_SHA256";

/// The runtime family the isolated door registers its one executor under.
///
/// Not [`crate::composition::LIVE_PROVIDER_RUNTIME`]: the door widens the
/// *home*, never the *provider* (§7.3 D9). `seal` refuses the live family
/// outright, and this door registers nothing else either.
pub(crate) const ISOLATED_RUNTIME: &str = "codex";

/// Why the isolated door refused. Every variant names one §7.3 row or the
/// bundle fact it could not establish.
#[derive(Debug, thiserror::Error)]
pub enum IsolatedHomeError {
    /// The bundle manifest could not be located, read, or parsed.
    #[error("the bundle manifest is unusable: {0}")]
    Manifest(&'static str),
    /// The bundled helper named by the manifest could not be read, or is not
    /// an exact mode-0700 regular file with one link inside the bundle root.
    #[error("the bundled helper is unusable: {0}")]
    Helper(&'static str),
    /// One of §7.3's D2-D7 refused.
    #[error(transparent)]
    Refused(#[from] ApplicationError),
    /// The minted mission identity was rejected by the core validator.
    #[error("the isolated door could not mint a mission identity")]
    MissionIdentity,
}

/// Everything seals 2-13 need, assembled by the door itself rather than handed
/// in by a caller.
pub(crate) struct IsolatedEnrollment {
    /// The admitted authority seals 4, 5, 6 and 12 are built from.
    pub(crate) authority: FreshFixtureAuthority,
    /// The vetted home, retained so seal 2's boundary can be minted from it.
    pub(crate) home: AuthorizedIsolatedRuntimeHome,
    /// Seal 5's label for the installed helper.
    pub(crate) helper_label: String,
    /// Seal 5's exact helper bytes, already attested against the manifest.
    pub(crate) helper_bytes: Vec<u8>,
    /// Seal 6's argument vector for the installed helper.
    pub(crate) helper_arguments: Vec<String>,
    /// The identity every row, event and receipt binds to.
    pub(crate) mission: MissionId,
}

/// The parsed bundle manifest.
struct BundleManifest {
    directory: PathBuf,
    helper: String,
    helper_digest: String,
    helper_arguments: Vec<String>,
}

impl BundleManifest {
    /// Reads the manifest sitting beside `argv[0]`.
    ///
    /// The executable path is canonicalized first, so a symlinked launcher
    /// resolves to the bundle it actually lives in rather than to wherever the
    /// link was planted.
    fn beside_executable() -> Result<Option<Self>, IsolatedHomeError> {
        let Ok(executable) = std::env::current_exe() else {
            return Err(IsolatedHomeError::Manifest(
                "argv[0] does not resolve to a path",
            ));
        };
        let Ok(executable) = fs::canonicalize(&executable) else {
            return Err(IsolatedHomeError::Manifest(
                "argv[0] does not resolve to an existing file",
            ));
        };
        let Some(directory) = executable.parent() else {
            return Err(IsolatedHomeError::Manifest("argv[0] has no directory"));
        };
        let path = directory.join(BUNDLE_MANIFEST);
        match fs::read_to_string(&path) {
            // A binary that is not running out of a bundle has no manifest.
            // That is not a refusal to report — it is simply not an enrolled
            // invocation, so the run stays `Unenrolled` exactly as it does when
            // the selector is unset.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(IsolatedHomeError::Manifest("the manifest cannot be read")),
            Ok(text) => Self::parse(directory, &text).map(Some),
        }
    }

    /// Parses the line-oriented manifest.
    ///
    /// Deliberately not JSON: `scripts/build-production-bundle.sh` writes it
    /// from shell, and a format a shell script can emit without a serializer is
    /// a format whose writer and reader cannot disagree about escaping. Blank
    /// lines and `#` comments are ignored; unknown keys are ignored so a later
    /// bundle can add fields without breaking an older binary's read.
    fn parse(directory: &Path, text: &str) -> Result<Self, IsolatedHomeError> {
        let mut helper = None;
        let mut helper_digest = None;
        let mut helper_arguments = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(IsolatedHomeError::Manifest(
                    "a manifest line is not key=value",
                ));
            };
            match key {
                "helper" => helper = Some(value.to_owned()),
                "helper_sha256" => helper_digest = Some(value.to_owned()),
                "helper_arg" => helper_arguments.push(value.to_owned()),
                _ => {}
            }
        }
        let (Some(helper), Some(helper_digest)) = (helper, helper_digest) else {
            return Err(IsolatedHomeError::Manifest(
                "the manifest names no helper and digest pair",
            ));
        };
        // One path component, no separators, no `..`: the helper ships beside
        // the binaries and the manifest cannot point outside the bundle.
        if helper.is_empty()
            || helper == "."
            || helper == ".."
            || helper.contains('/')
            || helper.contains('\0')
        {
            return Err(IsolatedHomeError::Manifest(
                "the manifest's helper name is not a single component",
            ));
        }
        Ok(Self {
            directory: directory.to_path_buf(),
            helper,
            helper_digest,
            helper_arguments,
        })
    }

    /// Reads the helper's bytes after checking its shape (§7.3 D8).
    fn helper_bytes(&self) -> Result<Vec<u8>, IsolatedHomeError> {
        let path = self.directory.join(&self.helper);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            return Err(IsolatedHomeError::Helper(
                "the manifest's helper is not present beside the binaries",
            ));
        };
        if !metadata.is_file() {
            return Err(IsolatedHomeError::Helper(
                "the manifest's helper is not a regular file",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() != 1 {
                return Err(IsolatedHomeError::Helper(
                    "the manifest's helper has more than one link",
                ));
            }
        }
        fs::read(&path).map_err(|_| IsolatedHomeError::Helper("the helper cannot be read"))
    }
}

/// Opens B5-DESIGN §7's door, or explains why it stays shut.
///
/// Returns `Ok(None)` for the two "the operator did not ask" cases — the
/// selector is not exactly `"1"` (D1), or this binary is not running out of a
/// bundle at all — because neither is an error: the run stays `Unenrolled` and
/// `orchestrator run` exits `ExecutionNotEnrolled` exactly as it does today.
/// Every other row of §7.3 is an `Err` naming what it checked, and every one of
/// them is decided **before any mutation** of the target home.
///
/// # Errors
/// Returns [`IsolatedHomeError`] for D2 through D8.
pub(crate) fn open(
    home: ResolvedRuntimeHome,
    guards: &IsolatedHomeGuards,
) -> Result<Option<IsolatedEnrollment>, IsolatedHomeError> {
    // D1 (and D10): the sole production selector, read inside
    // `orchestrator-app` beside the live-home selector it excludes.
    let Some(capability) = IsolatedHomeCanaryCapability::for_isolated_enrollment() else {
        return Ok(None);
    };
    let Some(manifest) = BundleManifest::beside_executable()? else {
        return Ok(None);
    };

    // D7/D8 first, so the bundle is proven before the home is touched at all.
    let helper_bytes = manifest.helper_bytes()?;
    attest_bundled_helper(
        &manifest.helper_digest,
        &helper_bytes,
        std::env::var(HELPER_DIGEST_CROSS_CHECK).ok().as_deref(),
    )?;

    // D2-D6, then admission under the unchanged fixture policy.
    let authorized = home.authorize_isolated(&capability, guards)?;
    let authority = authorized.admit(&helper_bytes, guards)?;

    Ok(Some(IsolatedEnrollment {
        mission: mint_mission_identity(authorized.path())?,
        helper_label: manifest.helper.clone(),
        helper_arguments: manifest.helper_arguments.clone(),
        helper_bytes,
        home: authorized,
        authority,
    }))
}

/// Mints this run's mission identity in Go's `YYYYMMDD-xxxxxxxx` shape.
///
/// Go derives the workspace id from the directory name it creates
/// (`internal/core/workspace.go:203`); the isolated door has no such directory
/// yet, so it mints one. The suffix is drawn from the process's own address
/// space layout and the home path rather than from a clock, so two runs started
/// in the same second do not collide.
fn mint_mission_identity(home: &Path) -> Result<MissionId, IsolatedHomeError> {
    use std::hash::{BuildHasher, Hash, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    home.hash(&mut hasher);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    let suffix = u32::try_from(hasher.finish() & 0xffff_ffff).unwrap_or(u32::MAX);
    let day = crate::composition::utc_date_stamp();
    MissionId::new(format!("{day}-{suffix:08x}")).map_err(|_| IsolatedHomeError::MissionIdentity)
}

#[cfg(test)]
mod tests {
    use super::{BundleManifest, IsolatedHomeError};
    use std::path::Path;

    #[test]
    fn a_manifest_without_a_digest_is_refused() {
        assert!(matches!(
            BundleManifest::parse(Path::new("/bundle"), "helper=orchestrator-helper\n"),
            Err(IsolatedHomeError::Manifest(_))
        ));
    }

    #[test]
    fn a_helper_name_that_is_not_one_component_is_refused() {
        for helper in ["../escape", "nested/helper", "..", "."] {
            let text = format!("helper={helper}\nhelper_sha256={}\n", "a".repeat(64));
            assert!(
                matches!(
                    BundleManifest::parse(Path::new("/bundle"), &text),
                    Err(IsolatedHomeError::Manifest(_))
                ),
                "helper {helper:?} must not be accepted"
            );
        }
    }

    #[test]
    fn unknown_keys_comments_and_blank_lines_are_ignored_and_arguments_accumulate() {
        let text = concat!(
            "# a comment\n",
            "\n",
            "bundle_id=whatever\n",
            "helper=orchestrator-local-protocol-helper\n",
            "helper_sha256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
            "helper_arg=--mode\n",
            "helper_arg=protocol\n",
        );
        let Ok(manifest) = BundleManifest::parse(Path::new("/bundle"), text) else {
            unreachable!("a well-formed manifest must parse")
        };
        assert_eq!(manifest.helper, "orchestrator-local-protocol-helper");
        assert_eq!(manifest.helper_arguments, vec!["--mode", "protocol"]);
    }
}
