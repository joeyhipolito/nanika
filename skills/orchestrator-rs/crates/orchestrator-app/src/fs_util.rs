use cap_primitives::fs::{FollowSymlinks, open_dir_nofollow};
use cap_std::fs::{Dir, OpenOptions};
use std::{
    io::{Read, Write},
    path::{Component, Path},
    sync::atomic::{AtomicU64, Ordering},
};

static NONCE: AtomicU64 = AtomicU64::new(1);

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct FileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

#[cfg(unix)]
pub(crate) fn identity(metadata: &cap_std::fs::Metadata) -> FileIdentity {
    use cap_std::fs::MetadataExt;
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

/// The kernel identity of whatever `path` resolves to, via [`std::fs::metadata`].
///
/// Symbolic links are followed, matching `metadata`'s own semantics: a caller
/// that also cares about the link itself compares canonical paths first, and a
/// link planted over a canonical root changes that path.
#[cfg(unix)]
pub(crate) fn path_identity(path: &Path) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::metadata(path)?;
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

pub(crate) fn mode(metadata: &cap_std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        metadata.mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        0
    }
}

pub(crate) fn link_count(metadata: &cap_std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        metadata.nlink()
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        1
    }
}

pub(crate) fn validate_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(crate) fn open_dir_path_nofollow(start: &Dir, path: &Path) -> std::io::Result<Dir> {
    let mut directory = start.try_clone()?.into_std_file();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => {
                directory = open_dir_nofollow(&directory, Path::new(name))?;
            }
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "directory path contains a non-normal component",
                ));
            }
        }
    }
    Ok(Dir::from_std_file(directory))
}

pub(crate) fn open_file_nofollow(
    directory: &Dir,
    name: &Path,
) -> std::io::Result<cap_std::fs::File> {
    let mut options = OpenOptions::new();
    options.read(true);
    options._cap_fs_ext_follow(FollowSymlinks::No);
    directory.open_with(name, &options)
}

pub(crate) fn probe_file_nofollow(directory: &Dir, name: &Path) -> std::io::Result<()> {
    let metadata = directory.symlink_metadata(name)?;
    if metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "refusing to follow a symbolic-link file entry",
        ));
    }
    Ok(())
}

pub(crate) fn read_bounded_nofollow(
    directory: &Dir,
    name: &Path,
    limit: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut file = match open_file_nofollow(directory, name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("entry is not a regular file"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(u64::try_from(limit).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds the capability size limit",
        ));
    }
    Ok(Some(bytes))
}

pub(crate) fn create_dir_private(directory: &Dir, name: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use cap_std::fs::{DirBuilder, DirBuilderExt, Permissions, PermissionsExt};
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        directory.create_dir_with(name, &builder)?;
        directory.set_permissions(name, Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        directory.create_dir(name)
    }
}

pub(crate) fn create_private_file(
    directory: &Dir,
    name: &Path,
    bytes: &[u8],
    executable: bool,
) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    options._cap_fs_ext_follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(if executable { 0o700 } else { 0o600 });
    }
    let mut file = directory.open_with(name, &options)?;
    #[cfg(unix)]
    {
        use cap_std::fs::{Permissions, PermissionsExt};
        file.set_permissions(Permissions::from_mode(if executable {
            0o700
        } else {
            0o600
        }))?;
    }
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result {
        let _ = directory.remove_file(name);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn atomic_replace_private(
    directory: &Dir,
    final_name: &Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    atomic_replace_file(directory, final_name, bytes, false)
}

pub(crate) fn atomic_replace_executable(
    directory: &Dir,
    final_name: &Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    atomic_replace_file(directory, final_name, bytes, true)
}

fn atomic_replace_file(
    directory: &Dir,
    final_name: &Path,
    bytes: &[u8],
    executable: bool,
) -> std::io::Result<()> {
    let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
    let file_name = final_name
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("atomic file name is not UTF-8"))?;
    let temporary = format!(".{file_name}.tmp-{}-{nonce}", std::process::id());
    let temporary = Path::new(&temporary);
    create_private_file(directory, temporary, bytes, executable)?;
    match directory.rename(temporary, directory, final_name) {
        Ok(()) => sync_dir(directory),
        Err(error) => {
            let _ = directory.remove_file(temporary);
            Err(error)
        }
    }
}

pub(crate) fn sync_dir(directory: &Dir) -> std::io::Result<()> {
    directory.try_clone()?.into_std_file().sync_all()
}

pub(crate) fn remove_tree_owned(parent: &Dir, name: &Path) -> std::io::Result<()> {
    let child = open_dir_path_nofollow(parent, name)?;
    for entry in child.entries()? {
        let entry = entry?;
        let file_name = entry.file_name();
        let path = Path::new(&file_name);
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            remove_tree_owned(&child, path)?;
        } else {
            child.remove_file(path)?;
        }
    }
    drop(child);
    parent.remove_dir(name)
}
