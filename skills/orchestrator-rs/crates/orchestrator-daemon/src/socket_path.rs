//! Short, private aliases for path-based AF_UNIX calls. Canonical authority stays
//! with the retained root; an alias never becomes a daemon discovery record.

use super::{
    DaemonError, FileIdentity, RootCapability, admit_existing_root, admit_existing_root_unsealed,
    io_error, remove_identity_checked, verify_root_capability,
};
use cap_std::fs::{MetadataExt as CapMetadataExt, PermissionsExt as CapPermissionsExt};
use rustix::fs::{Mode, RenameFlags, mkdirat, renameat_with, symlinkat};
use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(super) struct SocketPath<'a> {
    root: &'a RootCapability,
    path: PathBuf,
    alias: Option<AliasDirectory>,
}

struct AliasDirectory {
    root: RootCapability,
    parent: RootCapability,
    name: String,
    link: Option<FileIdentity>,
}

impl<'a> SocketPath<'a> {
    pub(super) fn new(root: &'a RootCapability, name: &str) -> Result<Self, DaemonError> {
        verify_root_capability(root)?;
        let path = root.path.join(name);
        // Leave room for the terminating NUL on every supported Unix platform.
        if path.as_os_str().len() < 100 {
            return Ok(Self {
                root,
                path,
                alias: None,
            });
        }
        #[cfg(target_os = "macos")]
        let parent = Path::new("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let parent = Path::new("/tmp");
        let parent = admit_existing_root_unsealed(parent)?;
        let parent_metadata = parent
            .directory
            .dir_metadata()
            .map_err(|source| io_error("inspect short socket parent", source))?;
        if !parent_metadata.is_dir()
            || parent_metadata.uid() != 0
            || parent_metadata.permissions().mode() & 0o1777 != 0o1777
        {
            return Err(DaemonError::UnsafeRoot);
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|error| {
            io_error(
                "generate socket alias name",
                io::Error::other(error.to_string()),
            )
        })?;
        let name = format!(
            ".nd-{}",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let alias_path = parent.path.join(&name);
        mkdirat(&parent.directory, name.as_str(), Mode::from_raw_mode(0o700))
            .map_err(|source| io_error("create private socket alias", source.into()))?;
        verify_root_capability(&parent)?;
        let mut alias = AliasDirectory {
            root: admit_existing_root(&alias_path)?,
            parent,
            name,
            link: None,
        };
        // The link is inside a fresh mode-0700 directory, outside the durable
        // writer's root. Only the AF_UNIX syscall traverses it.
        symlinkat(&root.path, &alias.root.directory, "r")
            .map_err(|source| io_error("create socket root alias", source.into()))?;
        let link = alias
            .root
            .directory
            .symlink_metadata("r")
            .map_err(|source| io_error("retain socket alias identity", source))?;
        alias.link = Some(FileIdentity::of_cap(&link));
        let result = Self {
            root,
            path: alias
                .root
                .path
                .join("r")
                .join(path.file_name().ok_or(DaemonError::UnsafeRoot)?),
            alias: Some(alias),
        };
        result.verify()?;
        Ok(result)
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn verify(&self) -> Result<(), DaemonError> {
        verify_root_capability(self.root)?;
        if let Some(alias) = &self.alias {
            verify_root_capability(&alias.parent)?;
            verify_root_capability(&alias.root)?;
            let link = alias
                .root
                .directory
                .symlink_metadata("r")
                .map_err(|source| io_error("verify socket alias identity", source))?;
            if !link.file_type().is_symlink() || Some(FileIdentity::of_cap(&link)) != alias.link {
                return Err(DaemonError::UnsafeRoot);
            }
            let target = fs::metadata(alias.root.path.join("r"))
                .map_err(|source| io_error("verify socket alias target", source))?;
            if !target.is_dir() || (target.dev(), target.ino()) != self.root.identity {
                return Err(DaemonError::UnsafeRoot);
            }
        }
        Ok(())
    }
}

impl Drop for AliasDirectory {
    fn drop(&mut self) {
        if verify_root_capability(&self.parent).is_err()
            || verify_root_capability(&self.root).is_err()
        {
            return;
        }
        if let Some(expected) = self.link {
            if remove_identity_checked(&self.root, "r", expected).is_err() {
                return;
            }
        }
        // Quarantine and compare before removing through the retained parent.
        // Never recursively delete or remove a replacement directory.
        let quarantine = format!("{}-remove", self.name);
        if renameat_with(
            &self.parent.directory,
            self.name.as_str(),
            &self.parent.directory,
            quarantine.as_str(),
            RenameFlags::NOREPLACE,
        )
        .is_err()
        {
            return;
        }
        if self
            .parent
            .directory
            .symlink_metadata(&quarantine)
            .is_ok_and(|metadata| {
                metadata.is_dir() && (metadata.dev(), metadata.ino()) == self.root.identity
            })
        {
            let _ = self.parent.directory.remove_dir(&quarantine);
        } else {
            let _ = renameat_with(
                &self.parent.directory,
                quarantine.as_str(),
                &self.parent.directory,
                self.name.as_str(),
                RenameFlags::NOREPLACE,
            );
        }
    }
}
