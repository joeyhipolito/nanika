//! Instructed Codex command wrappers, validated after execution. This is not tool interception.
use std::collections::BTreeSet;
use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{codex::CommandEvidence, portal_command};

type Result<T> = std::result::Result<T, String>;
const BUDGET: usize = 16 * 1024;
const MAX_LOG_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_COMMANDS: usize = 256;
const REVISION: &str = "portal-output/v2";

pub(crate) struct Portal {
    helper: PathBuf,
    helper_sha256: String,
    logs: PathBuf,
    directory: File,
    directory_identity: (u64, u64),
}

fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|error| error.to_string())
}

fn helper_path() -> Result<PathBuf> {
    let executable = io(fs::canonicalize(io(std::env::current_exe())?))?;
    Ok(executable
        .parent()
        .ok_or("executable has no parent")?
        .join("orchestrator-output"))
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

pub(crate) fn policy(workspace: &Path) -> Result<String> {
    let logs = workspace
        .parent()
        .ok_or("workspace has no parent")?
        .join("portal-logs");
    let helper = helper_path()?;
    let helper = helper.to_str().ok_or("helper path is not UTF-8")?;
    let log = logs.join("command-1.log");
    let log = log.to_str().ok_or("log path is not UTF-8")?;
    Ok(format!(
        "Portal output cap is ON for this standalone Codex coding attempt. Every shell tool command MUST be one literal invocation with this exact argument shape:\nexec {} --portal on --output-cap on --log {} -- /bin/sh -c 'YOUR COMMAND'\nUse a fresh command-N.log filename for each call. Put the entire command, including pipes or redirects, inside the final quoted script argument. Never run shell commands before or after the wrapper. Set max_output_tokens to 20000 so the complete <=16384-byte JSON response survives tool capture. Preserve failures; the helper returns the real exit code. Full logs remain available for targeted reads: use the wrapper again with dd or sed to read a bounded range, never load an entire large log. Do not edit logs or receipts. The only exception to the workspace boundary is executing this exact helper and reading/writing the specified portal-logs directory; source changes, and all other reads/writes, stay within the workspace. No tests, builds, linters, formatters, package managers, Git or network commands are authorized. Unwrapped or truncated outputs fail this attempt at post-run validation.\n\n",
        quote(helper),
        quote(log)
    ))
}

pub(crate) fn require_complete_capture(summary: &Value) -> Result<()> {
    if summary["stdout_discarded_bytes"].as_u64() != Some(0)
        || summary["cleanup_complete"].as_bool() != Some(true)
    {
        return Err(
            "Portal requires complete provider stdout and completed process cleanup".to_owned(),
        );
    }
    Ok(())
}

pub(crate) fn unavailable(status: &str, reason: &str) -> Value {
    json!({"schema":"nanika.portal-application.v1", "requested":"on", "applied":false,
        "status":status, "reason":reason, "verified_wrapped_calls":0,
        "observed_command_count":null,
        "mechanism":"instructed-wrapper-with-post-run-validation",
        "prevents_unwrapped_output_before_validation":false})
}

impl Portal {
    pub(crate) fn prepare(root: &Path) -> Result<Self> {
        let helper = helper_path()?;
        let helper_sha256 = fingerprint(&helper, false, MAX_LOG_BYTES)?.1;
        let logs = root.join("portal-logs");
        if helper.starts_with(root) {
            return Err("Portal helper must be outside the writable run directory".to_owned());
        }
        io(DirBuilder::new().mode(0o700).create(&logs))?;
        let directory = io(OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::DIRECTORY).bits() as i32,
            )
            .open(&logs))?;
        let metadata = io(directory.metadata())?;
        if metadata.mode() & 0o777 != 0o700 || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err("Portal log directory is not private and owned".to_owned());
        }
        Ok(Self {
            helper,
            helper_sha256,
            logs,
            directory,
            directory_identity: (metadata.dev(), metadata.ino()),
        })
    }

    fn check_directory(&self) -> Result<()> {
        let metadata = io(fs::symlink_metadata(&self.logs))?;
        let retained = io(self.directory.metadata())?;
        if !metadata.is_dir()
            || metadata.mode() & 0o777 != 0o700
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || (metadata.dev(), metadata.ino()) != self.directory_identity
            || (retained.dev(), retained.ino()) != self.directory_identity
        {
            return Err("Portal log directory changed or lost its private identity".to_owned());
        }
        Ok(())
    }

    pub(crate) fn validate(&self, commands: &[CommandEvidence]) -> Result<Value> {
        self.check_directory()?;
        if commands.len() > MAX_COMMANDS {
            return Err("Portal command count exceeds 256".to_owned());
        }
        if fingerprint(&self.helper, false, MAX_LOG_BYTES)?.1 != self.helper_sha256 {
            return Err("Portal helper changed during execution".to_owned());
        }
        let mut expected_names = BTreeSet::new();
        let mut rows = Vec::new();
        let mut total_bytes = 0_u64;
        let mut returned_bytes = 0_u64;
        for command in commands {
            let invocation = portal_command::parse(&command.command, &self.helper, &self.logs)?;
            let name = invocation
                .log
                .file_name()
                .ok_or("log name missing")?
                .to_owned();
            if !expected_names.insert(name) {
                return Err("Portal log name reused".to_owned());
            }
            let sidecar = PathBuf::from(format!("{}.portal.json", invocation.log.display()));
            expected_names.insert(
                sidecar
                    .file_name()
                    .ok_or("receipt name missing")?
                    .to_owned(),
            );
            if command.output.len() > BUDGET {
                return Err("Portal command response exceeds 16384 bytes".to_owned());
            }
            let response: Response = serde_json::from_str(&command.output)
                .map_err(|e| format!("Portal response is incomplete or invalid: {e}"))?;
            let receipt: Receipt = serde_json::from_slice(&read_private(&sidecar, 64 * 1024)?)
                .map_err(|e| format!("Portal receipt is incomplete or invalid: {e}"))?;
            let expected_path = invocation.log.to_str().ok_or("non-UTF8 log path")?;
            if response.revision != REVISION
                || receipt.revision != REVISION
                || response.controls_revision != "portal-controls/v1"
                || receipt.controls_revision != "portal-controls/v1"
                || response.full_log != expected_path
                || receipt.full_log != expected_path
                || response.command_exit_code != command.exit_code
                || receipt.command_exit_code != command.exit_code
                || response.response_budget_bytes != BUDGET
                || receipt.response_budget_bytes != BUDGET
                || receipt.state != "completed"
                || receipt.delivery_error.is_some()
                || response.log_bytes != receipt.log_bytes
                || receipt.returned_bytes != command.output.len() as u64
                || response.summary.full_log_sha256 != receipt.full_log_sha256
                || !response.modes.valid()
                || !receipt.modes.valid()
            {
                return Err("Portal output, receipt, and command do not agree".to_owned());
            }
            total_bytes = total_bytes
                .checked_add(response.log_bytes)
                .ok_or("Portal byte count overflow")?;
            if total_bytes > MAX_TOTAL_BYTES {
                return Err("Portal logs exceed the 512 MiB validation budget".to_owned());
            }
            let (bytes, digest) = fingerprint(&invocation.log, true, MAX_LOG_BYTES)?;
            if bytes != response.log_bytes || digest != response.summary.full_log_sha256 {
                return Err(
                    "Portal full log changed or does not match its response digest".to_owned(),
                );
            }
            returned_bytes = returned_bytes
                .checked_add(receipt.returned_bytes)
                .ok_or("Portal returned byte overflow")?;
            rows.push(json!({"command_id":command.id, "full_log":invocation.log,
                "full_log_sha256":digest, "full_log_bytes":bytes,"returned_bytes":receipt.returned_bytes,
                "command_exit_code":command.exit_code}));
        }
        let mut remaining_names = expected_names;
        for entry in io(fs::read_dir(&self.logs))? {
            if !remaining_names.remove(&io(entry)?.file_name()) {
                return Err("Portal log directory has unexpected artifacts".to_owned());
            }
        }
        if !remaining_names.is_empty() {
            return Err("Portal log directory has missing artifacts".to_owned());
        }
        self.check_directory()?;
        Ok(
            json!({"schema":"nanika.portal-application.v1", "requested":"on", "applied":!commands.is_empty(),
            "status":if commands.is_empty() {"no-shell-calls"} else {"observed"},
            "verified_wrapped_calls":commands.len(), "observed_command_count":commands.len(),
            "helper":self.helper,"helper_sha256":self.helper_sha256,"helper_revision":REVISION,
            "full_log_bytes":total_bytes,"returned_bytes":returned_bytes,"commands":rows,
            "mechanism":"instructed-wrapper-with-post-run-validation",
            "prevents_unwrapped_output_before_validation":false}),
        )
    }
}

#[derive(Deserialize)]
struct Modes {
    requested_portal: String,
    effective_portal: String,
    requested_output_cap: String,
    effective_output_cap: String,
    mode_source: String,
}
impl Modes {
    fn valid(&self) -> bool {
        self.requested_portal == "on"
            && self.effective_portal == "on"
            && self.requested_output_cap == "on"
            && self.effective_output_cap == "on"
            && self.mode_source == "explicit-output-cap"
    }
}
#[derive(Deserialize)]
struct Response {
    revision: String,
    controls_revision: String,
    #[serde(flatten)]
    modes: Modes,
    command_exit_code: i32,
    full_log: String,
    log_bytes: u64,
    response_budget_bytes: usize,
    summary: OutputSummary,
}
#[derive(Deserialize)]
struct OutputSummary {
    full_log_sha256: String,
}
#[derive(Deserialize)]
struct Receipt {
    revision: String,
    controls_revision: String,
    #[serde(flatten)]
    modes: Modes,
    command_exit_code: i32,
    full_log: String,
    log_bytes: u64,
    response_budget_bytes: usize,
    state: String,
    returned_bytes: u64,
    full_log_sha256: String,
    delivery_error: Option<String>,
}

fn open_regular(path: &Path, private: bool, limit: u64) -> Result<(File, Metadata)> {
    let file = io(OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path))?;
    let metadata = io(file.metadata())?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.len() > limit
        || (private && (metadata.mode() & 0o777 != 0o600 || metadata.nlink() != 1))
        || (!private && (metadata.mode() & 0o022 != 0 || metadata.mode() & 0o111 == 0))
    {
        return Err(
            "Portal artifact is not an owned, bounded regular file with expected permissions"
                .to_owned(),
        );
    }
    Ok((file, metadata))
}
fn signature(metadata: &Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}
fn stable(file: &File, path: &Path, before: &Metadata) -> Result<()> {
    if signature(&io(file.metadata())?) != signature(before)
        || signature(&io(fs::symlink_metadata(path))?) != signature(before)
    {
        return Err("Portal artifact changed during validation".to_owned());
    }
    Ok(())
}
fn fingerprint(path: &Path, private: bool, limit: u64) -> Result<(u64, String)> {
    let (mut file, before) = open_regular(path, private, limit)?;
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut chunk = [0_u8; 8192];
    loop {
        let n = io(file.read(&mut chunk))?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > limit {
            return Err("Portal file grew beyond validation budget".to_owned());
        }
        digest.update(&chunk[..n]);
    }
    stable(&file, path, &before)?;
    Ok((bytes, format!("{:x}", digest.finalize())))
}
fn read_private(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let (mut file, before) = open_regular(path, true, limit)?;
    let mut bytes = Vec::new();
    io((&mut file).take(limit + 1).read_to_end(&mut bytes))?;
    if bytes.len() as u64 > limit {
        return Err("Portal receipt grew beyond validation budget".to_owned());
    }
    stable(&file, path, &before)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_or_unquiesced_capture_cannot_establish_application() {
        for summary in [
            json!({}),
            json!({"stdout_discarded_bytes":1,"cleanup_complete":true}),
            json!({"stdout_discarded_bytes":0,"cleanup_complete":false}),
            json!({"stdout_discarded_bytes":0}),
            json!({"cleanup_complete":true}),
        ] {
            assert!(require_complete_capture(&summary).is_err());
        }
        assert!(
            require_complete_capture(&json!({"stdout_discarded_bytes":0,"cleanup_complete":true}))
                .is_ok()
        );
    }
}
