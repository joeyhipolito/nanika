//! Capability-root abstraction shared by fixture and production boundaries.
//!
//! [`CapabilityRoot`] is the crate-private, object-safe root of every admitted
//! capability. Only this crate's [`crate::ProductionBoundary`] and fixture
//! boundary types may implement or access it. Live production boundaries
//! require a retained, enrolled [`crate::ProductionWriterAuthority`]; the
//! private verification feature also has a disposable hermetic-canary minting
//! path whose paired roots remain bounded by a fixture lock and identity
//! manifest.
//!
//! Every admitted authority (workspace, target, executable, settings overlay)
//! holds a [`SharedCapabilityRoot`] pointing at the canonical root that bounds
//! its filesystem mutations. Keeping the raw directory handle crate-private is
//! essential: cloning that handle outside the crate would let it outlive the
//! writer lease and bypass the mutation actors.

use cap_primitives::fs::FollowSymlinks;
use cap_std::fs::{Dir, OpenOptions};
use std::{
    fmt, io,
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

use crate::runtime_home::{IsolatedFixtureRoot, ProductionBoundary};

/// Failures reported by a capability root while re-verifying its boundary.
#[derive(Debug, Error)]
pub enum CapabilityError {
    /// The canonical root identity changed between admission and use.
    #[error("capability root identity changed during use")]
    IdentityChanged,
    /// The root was adopted from an existing directory rather than created by
    /// the fixture harness, so it carries no proof of disposability.
    #[error("capability root was not freshly created by the fixture harness")]
    RootNotFreshlyCreated,
    /// The root was freshly created, so it carries no durable state a crashed
    /// predecessor could have left. Reported only by the fixture recovery
    /// door, which is the complement of the fresh one and never a substitute
    /// for it.
    #[error("capability root was freshly created and has nothing to recover")]
    RootFreshlyCreated,
    /// A filesystem operation against the canonical root failed.
    #[error("capability root filesystem operation failed: {0}")]
    Io(#[from] io::Error),
}

/// Failures acquiring a single-owner lease.
///
/// Kept separate from [`CapabilityError`] on purpose: that enum is matched
/// exhaustively by several authorities, and a single-owner lease is a narrower
/// concern than "a capability root misbehaved".
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OwnerLeaseError {
    /// Another live holder already owns this home's lease.
    ///
    /// Carries no path: the caller already named the home by authority.
    #[error("{lease} lease for this runtime home is already held")]
    Held {
        /// Stable name of the contended lease.
        lease: &'static str,
    },
    /// The capability root could not be verified.
    #[error(transparent)]
    Capability(#[from] CapabilityError),
    /// The lock file could not be opened.
    #[error("owner lease filesystem operation failed: {0}")]
    Io(#[from] io::Error),
}

/// Sealed marker supertrait. It is `pub(crate)` so it can be named by sibling
/// modules in this crate that implement [`CapabilityRoot`], but it is invisible
/// to external crates, which therefore cannot implement [`CapabilityRoot`].
pub(crate) mod private {
    /// Marker implemented only by this crate's own capability roots.
    pub trait Sealed {}
}

/// Shared, object-safe, sealed root of every admitted capability.
///
/// Implementors carry the canonical path, an open directory handle bounded to
/// that path, and a re-verification routine that detects replacement or
/// tampering. The trait is [`Send`] + [`Sync`] so authorities holding a shared
/// root may cross threads. Because [`CapabilityRoot`] requires the
/// crate-private [`private::Sealed`] supertrait, no type outside this crate can
/// implement it, so a [`SharedCapabilityRoot`] can never point at a forged root.
pub(crate) trait CapabilityRoot: Send + Sync + private::Sealed {
    /// Re-opens the canonical root and confirms its identity has not changed.
    fn verify(&self) -> Result<(), CapabilityError>;
    /// The open directory handle for the canonical root.
    fn directory(&self) -> &Dir;
    /// The canonical absolute path of the root.
    fn canonical_path(&self) -> &Path;
}

/// Convenience alias for the shared, type-erased, sealed capability root used by
/// every capability-holding authority.
pub(crate) type SharedCapabilityRoot = Arc<dyn CapabilityRoot>;

/// Authority to perform the two **declared additive** Go-store writes
/// (`learnings` row insert, `MEMORY_NEW.md` append) inside a disposable
/// fixture root.
///
/// This is the mintable half of B3-DESIGN §6. It carries the canonical root
/// that bounds the write, and the only way to obtain one is
/// [`Self::in_fixture`], which takes an [`IsolatedFixtureRoot`]. There is no
/// constructor that accepts a filesystem path, and no way to mint one against a
/// live home — so the additive write paths cannot be aimed at production even
/// by a caller inside this crate.
///
/// The grant is non-`Clone` and non-`Copy` by design: it is passed by reference
/// to each write, never stored as ambient authority.
///
/// The directory-swap detection this grant relies on ([`Self::verify`], via
/// the `identity` field's `(dev, ino)` comparison) is unix-only: on other
/// targets the field does not exist and re-verification falls back to the
/// canonicalized path alone, which cannot distinguish one real directory
/// from another swapped in at the same path.
pub struct AdditiveFixtureGrant {
    root: PathBuf,
    /// The root directory's kernel identity, recorded at mint time.
    ///
    /// Canonicalization cannot see one real directory swapped for another at
    /// the same path: both resolve to the same string. The `(dev, ino)` pair
    /// can — it is the kernel's own name for the object, and a replacement
    /// always gets a fresh one.
    #[cfg(unix)]
    identity: crate::fs_util::FileIdentity,
}

impl AdditiveFixtureGrant {
    /// Mints a grant bounded to one disposable fixture root.
    ///
    /// The root must have come from [`IsolatedFixtureRoot::create_fresh`].
    /// [`IsolatedFixtureRoot::identify`] names any existing directory — a live
    /// home included — so accepting one would let the additive write paths be
    /// aimed at a tree this process neither created nor owns. Provenance is
    /// checked before any filesystem work, so a refused mint touches nothing.
    ///
    /// # Errors
    /// Returns [`CapabilityError::RootNotFreshlyCreated`] when the root was
    /// adopted rather than created, [`CapabilityError::Io`] when the root
    /// cannot be re-opened or stat'd, and [`CapabilityError::IdentityChanged`]
    /// when canonicalizing it yields a different path than the authority
    /// reported.
    pub fn in_fixture(fixture: &IsolatedFixtureRoot) -> Result<Self, CapabilityError> {
        if !fixture.freshly_created() {
            return Err(CapabilityError::RootNotFreshlyCreated);
        }
        let root = std::fs::canonicalize(fixture.path())?;
        if root != fixture.path() {
            return Err(CapabilityError::IdentityChanged);
        }
        #[cfg(unix)]
        let identity = crate::fs_util::path_identity(&root)?;
        Ok(Self {
            root,
            #[cfg(unix)]
            identity,
        })
    }

    /// The canonical root this grant bounds writes to.
    ///
    /// Crate-private: an out-of-crate holder can pass the grant to an additive
    /// write but can never read the path back out of it.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Re-confirms the root's identity has not changed since minting.
    ///
    /// Two checks, because either alone is defeatable. The canonical path
    /// catches a symbolic link planted over the root; the recorded `(dev, ino)`
    /// catches the real directory being moved aside and a different real
    /// directory taking its name, which canonicalizes identically.
    ///
    /// # Errors
    /// Returns [`CapabilityError::IdentityChanged`] when the path now resolves
    /// elsewhere or the root is no longer the same directory object, and
    /// [`CapabilityError::Io`] when it cannot be resolved.
    pub(crate) fn verify(&self) -> Result<(), CapabilityError> {
        if std::fs::canonicalize(&self.root)? != self.root {
            return Err(CapabilityError::IdentityChanged);
        }
        #[cfg(unix)]
        if crate::fs_util::path_identity(&self.root)? != self.identity {
            return Err(CapabilityError::IdentityChanged);
        }
        Ok(())
    }
}

impl fmt::Debug for AdditiveFixtureGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdditiveFixtureGrant")
            .field("kind", &"additive-fixture-grant")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Single-owner leases (B3-DESIGN §6)
// ---------------------------------------------------------------------------

/// Lock file backing [`MetricsOwnerCapability`].
pub(crate) const METRICS_OWNER_LOCK_FILE: &str = "orchestrator.metrics-owner.lock";

/// An exclusive kernel lock held for the life of the guard.
///
/// `flock` associates a lock with the *open file description*, not the process,
/// so a second `open` + `LOCK_EX|LOCK_NB` in the same process contends exactly
/// as a second process would. That is what makes "there is one metrics owner"
/// provable in a single-process test rather than only across processes.
struct KernelLeaseGuard {
    #[allow(dead_code, reason = "the lock is released when the file is dropped")]
    file: cap_std::fs::File,
}

impl KernelLeaseGuard {
    fn acquire(directory: &Dir, name: &str, lease: &'static str) -> Result<Self, OwnerLeaseError> {
        let path = Path::new(name);
        let mut create = OpenOptions::new();
        create.read(true).write(true).create(true);
        create._cap_fs_ext_follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            create.mode(0o600);
        }
        let file = directory.open_with(path, &create)?;
        acquire_exclusive(&file, lease)?;
        Ok(Self { file })
    }
}

#[cfg(unix)]
fn acquire_exclusive(file: &cap_std::fs::File, lease: &'static str) -> Result<(), OwnerLeaseError> {
    rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(|error| {
        let source = io::Error::from(error);
        if source.kind() == io::ErrorKind::WouldBlock {
            OwnerLeaseError::Held { lease }
        } else {
            OwnerLeaseError::Io(source)
        }
    })
}

#[cfg(not(unix))]
fn acquire_exclusive(
    _file: &cap_std::fs::File,
    _lease: &'static str,
) -> Result<(), OwnerLeaseError> {
    Err(OwnerLeaseError::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        "single-owner leases require an operating-system file lock",
    )))
}

/// Sole authority to write one runtime home's `metrics.db` (B3-DESIGN §4).
///
/// Holding this capability *is* holding the lease: the guard releases only when
/// the capability drops, so "there is exactly one metrics owner per home" is an
/// operating-system fact rather than a convention. There is no constructor that
/// accepts a filesystem path — the home is named by an authority, and the path
/// is derived here.
pub struct MetricsOwnerCapability {
    boundary: Arc<ProductionBoundary>,
    #[allow(dead_code, reason = "held for its Drop; the lease is the capability")]
    lease: KernelLeaseGuard,
}

impl MetricsOwnerCapability {
    /// Assumes metrics ownership under the retained whole-home writer lease.
    ///
    /// # Errors
    /// Returns [`OwnerLeaseError::Held`] when another owner is live, and
    /// [`OwnerLeaseError::Io`] when the lock file cannot be opened.
    pub fn under_writer(
        writer: &crate::writer_authority::ProductionWriterAuthority,
    ) -> Result<Self, OwnerLeaseError> {
        Self::assume(writer.boundary())
    }

    /// Fixture-only door, gated exactly like
    /// [`crate::open_fixture_runtime_store`].
    ///
    /// A production `ProductionWriterAuthority` needs a live home, a quiescence
    /// proof, and the whole-home lease, none of which a disposable fixture can
    /// present. This door takes a [`ProductionBoundary`] built from an isolated
    /// fixture root, so it still cannot be aimed at a live home, and it is
    /// compiled out of every production build.
    ///
    /// # Errors
    /// Returns [`OwnerLeaseError::Held`] when another owner is live.
    #[cfg(any(test, feature = "test-support"))]
    pub fn in_fixture_boundary(boundary: Arc<ProductionBoundary>) -> Result<Self, OwnerLeaseError> {
        Self::assume(boundary)
    }

    /// The isolated-home door's metrics mint (B5-DESIGN §7).
    ///
    /// Crate-private and reachable only from
    /// [`crate::AuthorizedIsolatedRuntimeHome`], which is itself reachable only
    /// through `IsolatedHomeCanaryCapability`. It takes a boundary, never a
    /// path, exactly as its fixture sibling does — the difference is that this
    /// one compiles into the shipped binary, because the campaign's whole claim
    /// is that the shipped artifact enrolled.
    pub(crate) fn in_isolated_boundary(
        boundary: Arc<ProductionBoundary>,
    ) -> Result<Self, OwnerLeaseError> {
        Self::assume(boundary)
    }

    fn assume(boundary: Arc<ProductionBoundary>) -> Result<Self, OwnerLeaseError> {
        boundary.verify()?;
        let lease = KernelLeaseGuard::acquire(
            boundary.directory(),
            METRICS_OWNER_LOCK_FILE,
            "metrics owner",
        )?;
        Ok(Self { boundary, lease })
    }

    /// The canonical home directory this capability owns metrics for.
    ///
    /// Crate-private, and resolved here rather than in the owner, because
    /// naming a path requires the sealed [`CapabilityRoot`] trait. An
    /// out-of-crate holder can pass the capability to the owner but can never
    /// read the path back out of it.
    pub(crate) fn home_path(&self) -> &Path {
        self.boundary.canonical_path()
    }

    /// Re-confirms the home has not changed identity since the lease was taken.
    ///
    /// # Errors
    /// Returns [`CapabilityError::IdentityChanged`] when the root now resolves
    /// elsewhere.
    pub(crate) fn verify(&self) -> Result<(), CapabilityError> {
        self.boundary.verify()
    }
}

impl fmt::Debug for MetricsOwnerCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MetricsOwnerCapability")
            .field("kind", &"metrics-owner")
            .finish()
    }
}

/// Authority to append to one runtime home's audit chain (B3-DESIGN §6).
///
/// Deliberately *not* a lease: the chain's append is a single transaction
/// against `runtime.db`, whose exclusivity the store already enforces, and the
/// chain is append-only in SQLite regardless of how many holders exist.
///
/// What this type provides is that appending *carries* a
/// [`crate::StorageActorAuthority`], whose constructor is crate-private. The
/// actor lives inside the capability rather than beside it, so there is no way
/// to hold the capability without holding the authority it stands for — and no
/// out-of-crate caller can obtain either, so none can write history at all.
pub struct AuditAppendCapability {
    actor: crate::runtime_store::StorageActorAuthority,
}

impl AuditAppendCapability {
    pub(crate) const fn assume(actor: crate::runtime_store::StorageActorAuthority) -> Self {
        Self { actor }
    }

    /// The storage actor this capability stands for.
    pub(crate) const fn actor(&self) -> &crate::runtime_store::StorageActorAuthority {
        &self.actor
    }
}

impl fmt::Debug for AuditAppendCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuditAppendCapability")
            .field("kind", &"audit-append")
            .finish()
    }
}
