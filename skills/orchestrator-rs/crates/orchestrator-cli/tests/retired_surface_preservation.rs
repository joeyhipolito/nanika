#![cfg(unix)]

use orchestrator_cli::{CliError, run_system};
use std::{
    fs,
    io::{self, ErrorKind},
    net::{Ipv4Addr, TcpListener},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const RETIRED_COMMANDS: &[&str] = &[
    "notify",
    "telegram",
    "discord",
    "plugin",
    "plugins",
    "plugin_query",
    "plugin_action",
    "vault",
    "zettel",
    "obsidian",
];

struct OwnedFixtureRoot {
    path: PathBuf,
    owner: Vec<u8>,
}

impl OwnedFixtureRoot {
    fn create() -> io::Result<Self> {
        let parent = std::env::temp_dir().canonicalize()?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        for attempt in 0..32_u8 {
            let path = parent.join(format!(
                "nanika-b1-retired-preservation-{}-{nonce}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
                    let owner = format!("owned-fixture-root:{nonce}:{attempt}\n").into_bytes();
                    let fixture = Self { path, owner };
                    fixture.write_file(Path::new(".owner"), &fixture.owner, 0o600)?;
                    return Ok(fixture);
                }
                Err(source) if source.kind() == ErrorKind::AlreadyExists => {}
                Err(source) => return Err(source),
            }
        }
        Err(io::Error::new(
            ErrorKind::AlreadyExists,
            "could not create a unique preservation fixture root",
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn create_dir(&self, relative: &Path, mode: u32) -> io::Result<()> {
        let path = self.path.join(relative);
        fs::create_dir(&path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }

    fn write_file(&self, relative: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        let path = self.path.join(relative);
        fs::write(&path, bytes)?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }
}

impl Drop for OwnedFixtureRoot {
    fn drop(&mut self) {
        let is_exact_owned_root = self
            .path
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            == std::env::temp_dir().canonicalize().ok()
            && fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
                metadata.file_type().is_dir() && !metadata.file_type().is_symlink()
            })
            && fs::read(self.path.join(".owner")).is_ok_and(|bytes| bytes == self.owner);
        if is_exact_owned_root {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct SnapshotEntry {
    relative: PathBuf,
    mode: u32,
    bytes: Option<Vec<u8>>,
}

fn snapshot(root: &Path) -> io::Result<Vec<SnapshotEntry>> {
    fn visit(root: &Path, relative: &Path, entries: &mut Vec<SnapshotEntry>) -> io::Result<()> {
        let path = root.join(relative);
        let mut children = fs::read_dir(&path)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            let child_relative = relative.join(child.file_name());
            let metadata = fs::symlink_metadata(child.path())?;
            let bytes = if metadata.file_type().is_file() {
                Some(fs::read(child.path())?)
            } else if metadata.file_type().is_dir() {
                None
            } else {
                return Err(io::Error::other(format!(
                    "unexpected fixture file type: {}",
                    child.path().display()
                )));
            };
            entries.push(SnapshotEntry {
                relative: child_relative.clone(),
                mode: metadata.permissions().mode() & 0o7777,
                bytes,
            });
            if metadata.file_type().is_dir() {
                visit(root, &child_relative, entries)?;
            }
        }
        Ok(())
    }

    let root_metadata = fs::symlink_metadata(root)?;
    let mut entries = vec![SnapshotEntry {
        relative: PathBuf::new(),
        mode: root_metadata.permissions().mode() & 0o7777,
        bytes: None,
    }];
    visit(root, Path::new(""), &mut entries)?;
    Ok(entries)
}

fn assert_listener_idle(listener: &TcpListener, command: &str) -> io::Result<()> {
    match listener.accept() {
        Err(source) if source.kind() == ErrorKind::WouldBlock => Ok(()),
        Ok(_) => Err(io::Error::other(format!(
            "retired command {command} connected to the fixture listener"
        ))),
        Err(source) => Err(source),
    }
}

#[test]
fn retired_and_excluded_commands_fail_before_external_effects()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = OwnedFixtureRoot::create()?;
    for (path, mode) in [
        ("config", 0o700),
        ("config/channels", 0o700),
        ("plugins", 0o700),
        ("plugins/retired", 0o700),
        ("vault", 0o700),
        ("dropped", 0o700),
        ("dropped/zettel", 0o700),
    ] {
        fixture.create_dir(Path::new(path), mode)?;
    }
    for (path, bytes, mode) in [
        (
            "config/channels/telegram.json",
            b"{\"channel_ids\":[\"preserve\"],\"unknown\":true}\n".as_slice(),
            0o600,
        ),
        (
            "config/channels/discord.json",
            b"{\"events\":[\"mission.completed\"],\"opaque\":7}\n".as_slice(),
            0o640,
        ),
        (
            "plugins/retired/manifest.json",
            b"{\"name\":\"synthetic-retired-fixture\",\"binary\":\"trap-plugin\"}\n".as_slice(),
            0o600,
        ),
        (
            "plugins/retired/auth.bin",
            b"synthetic-auth\0preserve-exactly".as_slice(),
            0o600,
        ),
        (
            "plugins/retired/cache.bin",
            b"synthetic-cache\r\nopaque\xff".as_slice(),
            0o640,
        ),
        (
            "vault/Home.md",
            b"# Synthetic preserved vault\n\nbyte exact\n".as_slice(),
            0o640,
        ),
        (
            "dropped/zettel/legacy.md",
            b"synthetic historical zettel\n".as_slice(),
            0o600,
        ),
    ] {
        fixture.write_file(Path::new(path), bytes, mode)?;
    }

    let process_marker = fixture.path().join("unexpected-process-authority");
    let trap = fixture.path().join("plugins/retired/trap-plugin");
    let script =
        b"#!/bin/sh\nprintf invoked > \"${0%/*}/../../unexpected-process-authority\"\nexit 97\n";
    fixture.write_file(Path::new("plugins/retired/trap-plugin"), script, 0o700)?;

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let root_argument = fixture.path().to_str().ok_or("non-UTF-8 fixture path")?;
    let trap_argument = trap.to_str().ok_or("non-UTF-8 trap path")?;
    let before = snapshot(fixture.path())?;

    for &command in RETIRED_COMMANDS {
        let mut output = Vec::new();
        let mut error_output = Vec::new();
        let error = match run_system(
            [
                command,
                "--fixture-root",
                root_argument,
                "--plugin-executable",
                trap_argument,
                "--endpoint",
                endpoint.as_str(),
            ],
            &mut output,
            &mut error_output,
        ) {
            Err(error) => error,
            Ok(()) => return Err(format!("retired command {command} was accepted").into()),
        };

        match error {
            CliError::UnknownCommand(refused) => assert_eq!(refused, command),
            other => {
                return Err(format!("{command} crossed the root parser gate: {other}").into());
            }
        }
        assert!(output.is_empty(), "{command} wrote command output");
        assert!(
            error_output.is_empty(),
            "{command} reached usage or runtime diagnostics"
        );
        assert!(
            !process_marker.exists(),
            "{command} invoked the synthetic process trap"
        );
        assert_listener_idle(&listener, command)?;
        assert_eq!(
            snapshot(fixture.path())?,
            before,
            "{command} changed fixture bytes, permissions, or entries"
        );
    }

    Ok(())
}
