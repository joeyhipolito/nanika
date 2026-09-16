use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use super::{
    ExecutableFileAttestation, ExecutableFileMode, ExecutableMetadataSnapshot, FileIdentity,
    MAX_ARGUMENT_BYTES, MAX_ARGUMENT_TOTAL, MAX_ARGUMENTS, MAX_ENVIRONMENT_ENTRIES,
    MAX_ENVIRONMENT_TOTAL, MAX_STDIN_BYTES, canonical_external_mapping,
    pinned_external_metadata_is_valid, validate_attested_executable_length,
    verify_admitted_executable_metadata, verify_executable_file_attestation,
};
use crate::gate::{GateChallenge, GateControl, LauncherGate};

const REQUEST_MAGIC: &[u8; 8] = b"NANBRK01";
// v4 binds the target policy and the two-stage grant/StartedObserved/final-START
// protocol. Older brokers either lack that policy or execute after one grant.
const REQUEST_VERSION_GATED: u32 = 4;
const GATE_CHALLENGE_BYTES: usize = 32;
const MAX_REQUEST_BYTES: usize =
    MAX_ARGUMENT_TOTAL + MAX_ENVIRONMENT_TOTAL + (MAX_ARGUMENTS * 4) + 8 * 1024;
const STATUS_MAGIC: &[u8; 8] = b"NANBST01";
const STATUS_LENGTH: u64 = 13;
const STATUS_STATE_OFFSET: u64 = 8;
const STATUS_VALUE_OFFSET: u64 = 9;

pub(crate) const REQUEST_PATH_ENV: &str = "NANIKA_BROKER_REQUEST_PATH";
pub(crate) const REQUEST_DEVICE_ENV: &str = "NANIKA_BROKER_REQUEST_DEVICE";
pub(crate) const REQUEST_INODE_ENV: &str = "NANIKA_BROKER_REQUEST_INODE";
pub(crate) const REQUEST_LENGTH_ENV: &str = "NANIKA_BROKER_REQUEST_LENGTH";
pub(crate) const STDIN_PATH_ENV: &str = "NANIKA_BROKER_STDIN_PATH";
pub(crate) const STDIN_DEVICE_ENV: &str = "NANIKA_BROKER_STDIN_DEVICE";
pub(crate) const STDIN_INODE_ENV: &str = "NANIKA_BROKER_STDIN_INODE";
pub(crate) const STDIN_LENGTH_ENV: &str = "NANIKA_BROKER_STDIN_LENGTH";
pub(crate) const STATUS_PATH_ENV: &str = "NANIKA_BROKER_STATUS_PATH";
pub(crate) const STATUS_DEVICE_ENV: &str = "NANIKA_BROKER_STATUS_DEVICE";
pub(crate) const STATUS_INODE_ENV: &str = "NANIKA_BROKER_STATUS_INODE";
pub(crate) const GATE_PATH_ENV: &str = "NANIKA_BROKER_GATE_PATH";
pub(crate) const GATE_DEVICE_ENV: &str = "NANIKA_BROKER_GATE_DEVICE";
pub(crate) const GATE_INODE_ENV: &str = "NANIKA_BROKER_GATE_INODE";

const CONTROL_ENVIRONMENT: [&str; 14] = [
    REQUEST_PATH_ENV,
    REQUEST_DEVICE_ENV,
    REQUEST_INODE_ENV,
    REQUEST_LENGTH_ENV,
    STDIN_PATH_ENV,
    STDIN_DEVICE_ENV,
    STDIN_INODE_ENV,
    STDIN_LENGTH_ENV,
    STATUS_PATH_ENV,
    STATUS_DEVICE_ENV,
    STATUS_INODE_ENV,
    GATE_PATH_ENV,
    GATE_DEVICE_ENV,
    GATE_INODE_ENV,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrokerStatus {
    Pending,
    Running,
    TargetExited(i32),
    TargetSignaled(i32),
    Failed,
}

impl BrokerStatus {
    const fn tag(self) -> u8 {
        match self {
            Self::Pending => b'P',
            Self::Running => b'R',
            Self::TargetExited(_) => b'X',
            Self::TargetSignaled(_) => b'S',
            Self::Failed => b'F',
        }
    }

    const fn value(self) -> i32 {
        match self {
            Self::TargetExited(value) | Self::TargetSignaled(value) => value,
            Self::Pending | Self::Running | Self::Failed => 0,
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn pending_status_image() -> [u8; STATUS_LENGTH as usize] {
    let mut image = [0; STATUS_LENGTH as usize];
    image[..STATUS_MAGIC.len()].copy_from_slice(STATUS_MAGIC);
    image[STATUS_STATE_OFFSET as usize] = BrokerStatus::Pending.tag();
    image
}

pub(crate) fn read_status(file: &File, identity: FileIdentity) -> std::io::Result<BrokerStatus> {
    verify_status_metadata(&file.metadata()?, identity)?;
    let mut image = [0; STATUS_LENGTH as usize];
    read_exact_at(file, &mut image, 0)?;
    let status = decode_status(&image)?;
    verify_status_metadata(&file.metadata()?, identity)?;
    Ok(status)
}

pub(crate) struct BrokerRequest<'a> {
    pub(crate) gate_challenge: GateChallenge,
    pub(crate) cwd_identity: FileIdentity,
    pub(crate) executable_path: &'a Path,
    pub(crate) executable_identity: FileIdentity,
    pub(crate) executable_length: u64,
    pub(crate) target_policy: BrokerTargetPolicy,
    pub(crate) arguments: &'a [OsString],
    pub(crate) environment: &'a [(OsString, OsString)],
}

#[derive(Clone, Copy)]
pub(crate) enum BrokerTargetPolicy {
    SealedClone,
    PinnedExternal {
        attestation: ExecutableFileAttestation,
        admitted_metadata: ExecutableMetadataSnapshot,
    },
}

pub(crate) fn encode_request(request: &BrokerRequest<'_>) -> std::io::Result<Vec<u8>> {
    let mut encoded = Vec::with_capacity(
        REQUEST_MAGIC.len()
            + MAX_ARGUMENT_TOTAL.min(4 * 1024)
            + MAX_ENVIRONMENT_TOTAL.min(4 * 1024),
    );
    encoded.extend_from_slice(REQUEST_MAGIC);
    push_u32(&mut encoded, REQUEST_VERSION_GATED);
    encoded.extend_from_slice(&request.gate_challenge.as_bytes());
    push_u64(&mut encoded, request.cwd_identity.device);
    push_u64(&mut encoded, request.cwd_identity.inode);
    push_u64(&mut encoded, request.executable_identity.device);
    push_u64(&mut encoded, request.executable_identity.inode);
    push_u64(&mut encoded, request.executable_length);
    match request.target_policy {
        BrokerTargetPolicy::SealedClone => encoded.push(1),
        BrokerTargetPolicy::PinnedExternal {
            attestation,
            admitted_metadata,
        } => {
            if attestation.length != request.executable_length
                || admitted_metadata.identity != request.executable_identity
                || admitted_metadata.length != request.executable_length
            {
                return Err(invalid_request());
            }
            encoded.push(2);
            push_u32(&mut encoded, admitted_metadata.mode);
            push_u32(&mut encoded, admitted_metadata.uid);
            push_u32(&mut encoded, admitted_metadata.gid);
            push_i64(&mut encoded, admitted_metadata.modified_seconds);
            push_i64(&mut encoded, admitted_metadata.modified_nanoseconds);
            push_i64(&mut encoded, admitted_metadata.changed_seconds);
            push_i64(&mut encoded, admitted_metadata.changed_nanoseconds);
            encoded.extend_from_slice(&attestation.sha256);
        }
    }
    push_bytes(&mut encoded, request.executable_path.as_os_str().as_bytes())?;
    push_count(&mut encoded, request.arguments.len())?;
    for argument in request.arguments {
        push_bytes(&mut encoded, argument.as_bytes())?;
    }
    push_count(&mut encoded, request.environment.len())?;
    for (key, value) in request.environment {
        push_bytes(&mut encoded, key.as_bytes())?;
        push_bytes(&mut encoded, value.as_bytes())?;
    }
    if encoded.len() > MAX_REQUEST_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "broker request exceeds its fixed bound",
        ));
    }
    Ok(encoded)
}

fn push_count(encoded: &mut Vec<u8>, value: usize) -> std::io::Result<()> {
    let value = u32::try_from(value)
        .map_err(|_| std::io::Error::other("broker request count exceeds its wire type"))?;
    push_u32(encoded, value);
    Ok(())
}

fn push_bytes(encoded: &mut Vec<u8>, value: &[u8]) -> std::io::Result<()> {
    push_count(encoded, value.len())?;
    encoded.extend_from_slice(value);
    Ok(())
}

fn push_u32(encoded: &mut Vec<u8>, value: u32) {
    encoded.extend_from_slice(&value.to_be_bytes());
}

fn push_u64(encoded: &mut Vec<u8>, value: u64) {
    encoded.extend_from_slice(&value.to_be_bytes());
}

fn push_i64(encoded: &mut Vec<u8>, value: i64) {
    encoded.extend_from_slice(&value.to_be_bytes());
}

struct DecodedRequest {
    gate_challenge: GateChallenge,
    cwd_identity: FileIdentity,
    executable_path: PathBuf,
    executable_identity: FileIdentity,
    executable_length: u64,
    target_policy: BrokerTargetPolicy,
    arguments: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
}

impl DecodedRequest {
    fn decode(encoded: &[u8]) -> std::io::Result<Self> {
        let mut decoder = Decoder::new(encoded);
        if decoder.take(REQUEST_MAGIC.len())? != REQUEST_MAGIC {
            return Err(invalid_request());
        }
        if decoder.u32()? != REQUEST_VERSION_GATED {
            return Err(invalid_request());
        }
        let gate_challenge = GateChallenge::from_bytes(
            decoder
                .take(GATE_CHALLENGE_BYTES)?
                .try_into()
                .map_err(|_| invalid_request())?,
        );
        let cwd_identity = FileIdentity {
            device: decoder.u64()?,
            inode: decoder.u64()?,
        };
        let executable_identity = FileIdentity {
            device: decoder.u64()?,
            inode: decoder.u64()?,
        };
        let executable_length = decoder.u64()?;
        let target_policy = match decoder.byte()? {
            1 => BrokerTargetPolicy::SealedClone,
            2 => {
                let admitted_metadata = ExecutableMetadataSnapshot {
                    identity: executable_identity,
                    length: executable_length,
                    mode: decoder.u32()?,
                    uid: decoder.u32()?,
                    gid: decoder.u32()?,
                    modified_seconds: decoder.i64()?,
                    modified_nanoseconds: decoder.i64()?,
                    changed_seconds: decoder.i64()?,
                    changed_nanoseconds: decoder.i64()?,
                };
                let sha256 = decoder
                    .take(32)?
                    .try_into()
                    .map_err(|_| invalid_request())?;
                let attestation = ExecutableFileAttestation::new(executable_length, sha256);
                validate_attested_executable_length(attestation).map_err(|_| invalid_request())?;
                BrokerTargetPolicy::PinnedExternal {
                    attestation,
                    admitted_metadata,
                }
            }
            _ => return Err(invalid_request()),
        };
        let executable_path = PathBuf::from(OsString::from_vec(decoder.bytes()?.to_vec()));
        if !executable_path.is_absolute() {
            return Err(invalid_request());
        }

        let argument_count = decoder.count(MAX_ARGUMENTS)?;
        let mut arguments = Vec::with_capacity(argument_count);
        let mut argument_total = 0usize;
        for _ in 0..argument_count {
            let argument = decoder.bytes()?;
            if argument.len() > MAX_ARGUMENT_BYTES || argument.contains(&0) {
                return Err(invalid_request());
            }
            argument_total = argument_total.saturating_add(argument.len());
            if argument_total > MAX_ARGUMENT_TOTAL {
                return Err(invalid_request());
            }
            arguments.push(OsString::from_vec(argument.to_vec()));
        }

        let environment_count = decoder.count(MAX_ENVIRONMENT_ENTRIES)?;
        let mut environment = Vec::with_capacity(environment_count);
        let mut environment_total = 0usize;
        for _ in 0..environment_count {
            let key = decoder.bytes()?;
            let value = decoder.bytes()?;
            if key.is_empty()
                || key.contains(&0)
                || key.contains(&b'=')
                || value.contains(&0)
                || CONTROL_ENVIRONMENT
                    .iter()
                    .any(|control| key == control.as_bytes())
            {
                return Err(invalid_request());
            }
            environment_total = environment_total
                .saturating_add(key.len())
                .saturating_add(value.len());
            if environment_total > MAX_ENVIRONMENT_TOTAL {
                return Err(invalid_request());
            }
            environment.push((
                OsString::from_vec(key.to_vec()),
                OsString::from_vec(value.to_vec()),
            ));
        }
        if !decoder.is_empty() {
            return Err(invalid_request());
        }
        Ok(Self {
            gate_challenge,
            cwd_identity,
            executable_path,
            executable_identity,
            executable_length,
            target_policy,
            arguments,
            environment,
        })
    }
}

struct Decoder<'a> {
    encoded: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    fn new(encoded: &'a [u8]) -> Self {
        Self { encoded, offset: 0 }
    }

    fn take(&mut self, length: usize) -> std::io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(invalid_request)?;
        let value = self
            .encoded
            .get(self.offset..end)
            .ok_or_else(invalid_request)?;
        self.offset = end;
        Ok(value)
    }

    fn u32(&mut self) -> std::io::Result<u32> {
        let bytes: [u8; 4] = self.take(4)?.try_into().map_err(|_| invalid_request())?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn byte(&mut self) -> std::io::Result<u8> {
        self.take(1)?.first().copied().ok_or_else(invalid_request)
    }

    fn u64(&mut self) -> std::io::Result<u64> {
        let bytes: [u8; 8] = self.take(8)?.try_into().map_err(|_| invalid_request())?;
        Ok(u64::from_be_bytes(bytes))
    }

    fn i64(&mut self) -> std::io::Result<i64> {
        let bytes: [u8; 8] = self.take(8)?.try_into().map_err(|_| invalid_request())?;
        Ok(i64::from_be_bytes(bytes))
    }

    fn count(&mut self, maximum: usize) -> std::io::Result<usize> {
        let value = usize::try_from(self.u32()?).map_err(|_| invalid_request())?;
        if value > maximum {
            return Err(invalid_request());
        }
        Ok(value)
    }

    fn bytes(&mut self) -> std::io::Result<&'a [u8]> {
        let length = self.count(MAX_REQUEST_BYTES)?;
        self.take(length)
    }

    fn is_empty(&self) -> bool {
        self.offset == self.encoded.len()
    }
}

fn invalid_request() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "invalid process broker request",
    )
}

struct ControlFile {
    path: PathBuf,
    identity: FileIdentity,
    length: u64,
}

impl ControlFile {
    fn from_environment(
        path_key: &str,
        device_key: &str,
        inode_key: &str,
        length_key: &str,
    ) -> std::io::Result<Option<Self>> {
        let Some(path) = std::env::var_os(path_key) else {
            if [device_key, inode_key, length_key]
                .iter()
                .any(|key| std::env::var_os(key).is_some())
            {
                return Err(invalid_control());
            }
            return Ok(None);
        };
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(invalid_control());
        }
        Ok(Some(Self {
            path,
            identity: FileIdentity {
                device: parse_control_u64(device_key)?,
                inode: parse_control_u64(inode_key)?,
            },
            length: parse_control_u64(length_key)?,
        }))
    }

    fn open(&self, maximum_length: usize) -> std::io::Result<File> {
        if self.length > u64::try_from(maximum_length).unwrap_or(u64::MAX) {
            return Err(invalid_control());
        }
        let file = File::from(
            rustix::fs::open(
                &self.path,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        verify_control_metadata(&file.metadata()?, self.identity, self.length)?;
        Ok(file)
    }
}

struct StatusControl {
    path: PathBuf,
    identity: FileIdentity,
}

struct GateEnvironment {
    path: PathBuf,
    identity: FileIdentity,
}

impl GateEnvironment {
    fn from_environment(required: bool) -> std::io::Result<Option<Self>> {
        let path = std::env::var_os(GATE_PATH_ENV);
        let any_identity = [GATE_DEVICE_ENV, GATE_INODE_ENV]
            .iter()
            .any(|key| std::env::var_os(key).is_some());
        let Some(path) = path else {
            if required || any_identity {
                return Err(invalid_control());
            }
            return Ok(None);
        };
        if !required || !any_identity {
            return Err(invalid_control());
        }
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(invalid_control());
        }
        Ok(Some(Self {
            path,
            identity: FileIdentity {
                device: parse_control_u64(GATE_DEVICE_ENV)?,
                inode: parse_control_u64(GATE_INODE_ENV)?,
            },
        }))
    }
}

impl StatusControl {
    fn from_environment() -> std::io::Result<Self> {
        let path = PathBuf::from(std::env::var_os(STATUS_PATH_ENV).ok_or_else(invalid_control)?);
        if !path.is_absolute() {
            return Err(invalid_control());
        }
        Ok(Self {
            path,
            identity: FileIdentity {
                device: parse_control_u64(STATUS_DEVICE_ENV)?,
                inode: parse_control_u64(STATUS_INODE_ENV)?,
            },
        })
    }

    fn open(&self) -> std::io::Result<File> {
        let file = File::from(
            rustix::fs::open(
                &self.path,
                rustix::fs::OFlags::RDWR
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        if read_status(&file, self.identity)? != BrokerStatus::Pending {
            return Err(invalid_control());
        }
        Ok(file)
    }
}

fn parse_control_u64(key: &str) -> std::io::Result<u64> {
    std::env::var_os(key)
        .and_then(|value| value.to_str().and_then(|value| value.parse().ok()))
        .ok_or_else(invalid_control)
}

fn invalid_control() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "invalid process broker control plane",
    )
}

fn verify_control_metadata(
    metadata: &Metadata,
    identity: FileIdentity,
    length: u64,
) -> std::io::Result<()> {
    if !metadata.is_file()
        || FileIdentity::of(metadata) != identity
        || metadata.len() != length
        || metadata.permissions().mode() & 0o777 != 0o400
    {
        return Err(invalid_control());
    }
    Ok(())
}

fn verify_status_metadata(metadata: &Metadata, identity: FileIdentity) -> std::io::Result<()> {
    if !metadata.is_file()
        || FileIdentity::of(metadata) != identity
        || metadata.len() != STATUS_LENGTH
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(invalid_control());
    }
    Ok(())
}

fn decode_status(image: &[u8; STATUS_LENGTH as usize]) -> std::io::Result<BrokerStatus> {
    if image.get(..STATUS_MAGIC.len()) != Some(STATUS_MAGIC) {
        return Err(invalid_control());
    }
    let value = i32::from_be_bytes(
        image[STATUS_VALUE_OFFSET as usize..]
            .try_into()
            .map_err(|_| invalid_control())?,
    );
    match (image[STATUS_STATE_OFFSET as usize], value) {
        (b'P', 0) => Ok(BrokerStatus::Pending),
        // Terminal transitions stage their value before committing the tag.
        // Readers may therefore observe `R` with a future exit code or signal;
        // the tag remains the linearization point and the staged value has no
        // meaning until it becomes `X` or `S`.
        (b'R', value) if value >= 0 => Ok(BrokerStatus::Running),
        (b'X', value) if value >= 0 => Ok(BrokerStatus::TargetExited(value)),
        (b'S', value) if value > 0 => Ok(BrokerStatus::TargetSignaled(value)),
        (b'F', 0) => Ok(BrokerStatus::Failed),
        _ => Err(invalid_control()),
    }
}

fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    while !buffer.is_empty() {
        let read = file.read_at(buffer, offset)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "broker status ended before its fixed bound",
            ));
        }
        let (_, remaining) = buffer.split_at_mut(read);
        buffer = remaining;
        offset = offset.saturating_add(read as u64);
    }
    Ok(())
}

fn write_status(file: &File, status: BrokerStatus) -> std::io::Result<()> {
    write_all_at(file, &status.value().to_be_bytes(), STATUS_VALUE_OFFSET)?;
    if file.write_at(&[status.tag()], STATUS_STATE_OFFSET)? != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "broker status transition was not recorded",
        ));
    }
    Ok(())
}

fn write_all_at(file: &File, mut bytes: &[u8], mut offset: u64) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let written = file.write_at(bytes, offset)?;
        if written == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "broker status transition was not recorded",
            ));
        }
        bytes = &bytes[written..];
        offset = offset.saturating_add(written as u64);
    }
    Ok(())
}

fn read_control_file(file: &mut File, maximum_length: usize) -> std::io::Result<Vec<u8>> {
    let length = usize::try_from(file.metadata()?.len()).map_err(|_| invalid_control())?;
    if length > maximum_length {
        return Err(invalid_control());
    }
    let mut bytes = Vec::with_capacity(length);
    file.read_to_end(&mut bytes)?;
    if bytes.len() != length {
        return Err(invalid_control());
    }
    Ok(bytes)
}

fn verify_target(
    path: &Path,
    identity: FileIdentity,
    length: u64,
    policy: BrokerTargetPolicy,
) -> std::io::Result<File> {
    let file = File::from(
        rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let metadata = file.metadata()?;
    match policy {
        BrokerTargetPolicy::SealedClone => {
            if !metadata.is_file()
                || FileIdentity::of(&metadata) != identity
                || metadata.len() != length
                || metadata.permissions().mode() & 0o111 == 0
                || metadata.permissions().mode() & 0o222 != 0
            {
                return Err(invalid_target());
            }
        }
        BrokerTargetPolicy::PinnedExternal {
            attestation,
            admitted_metadata,
        } => {
            if !pinned_external_metadata_is_valid(&metadata, rustix::process::geteuid().as_raw())
                || !canonical_external_mapping(path, identity)?
            {
                return Err(invalid_target());
            }
            verify_admitted_executable_metadata(
                &file,
                identity,
                attestation,
                admitted_metadata,
                ExecutableFileMode::PinnedExternal,
            )?;
            let verified = verify_executable_file_attestation(
                &file,
                identity,
                attestation,
                ExecutableFileMode::PinnedExternal,
            )?;
            if verified != admitted_metadata || !canonical_external_mapping(path, identity)? {
                return Err(invalid_target());
            }
        }
    }
    Ok(file)
}

fn invalid_target() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "process broker target identity changed",
    )
}

fn verify_control_environment() -> std::io::Result<()> {
    if std::env::args_os().count() != 1 {
        return Err(invalid_control());
    }
    if std::env::vars_os().any(|(key, _)| {
        !CONTROL_ENVIRONMENT
            .iter()
            .any(|allowed| key == OsStr::new(allowed))
    }) {
        return Err(invalid_control());
    }
    Ok(())
}

fn run_broker() -> std::io::Result<()> {
    verify_control_environment()?;
    let status_control = StatusControl::from_environment()?;
    let status_file = status_control.open()?;
    let request_control = ControlFile::from_environment(
        REQUEST_PATH_ENV,
        REQUEST_DEVICE_ENV,
        REQUEST_INODE_ENV,
        REQUEST_LENGTH_ENV,
    )?
    .ok_or_else(invalid_control)?;
    let mut request_file = request_control.open(MAX_REQUEST_BYTES)?;
    let request =
        DecodedRequest::decode(&read_control_file(&mut request_file, MAX_REQUEST_BYTES)?)?;
    verify_control_metadata(
        &request_file.metadata()?,
        request_control.identity,
        request_control.length,
    )?;

    let cwd = File::from(rustix::io::dup(std::io::stdin()).map_err(std::io::Error::from)?);
    let cwd_metadata = cwd.metadata()?;
    if !cwd_metadata.is_dir() || FileIdentity::of(&cwd_metadata) != request.cwd_identity {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "process broker CWD identity changed",
        ));
    }

    let executable = verify_target(
        &request.executable_path,
        request.executable_identity,
        request.executable_length,
        request.target_policy,
    )?;
    let stdin_control = ControlFile::from_environment(
        STDIN_PATH_ENV,
        STDIN_DEVICE_ENV,
        STDIN_INODE_ENV,
        STDIN_LENGTH_ENV,
    )?;
    let stdin = match &stdin_control {
        Some(control) => Some(control.open(MAX_STDIN_BYTES)?),
        None => None,
    };
    let gate_environment = GateEnvironment::from_environment(true)?.ok_or_else(invalid_control)?;
    let gate = LauncherGate::connect(
        &GateControl::from_parts(
            gate_environment.path,
            gate_environment.identity,
            request.gate_challenge,
        ),
        std::process::id(),
    )?;

    let mut command = Command::new(&request.executable_path);
    command.args(&request.arguments);
    command.env_clear();
    for (key, value) in &request.environment {
        command.env(key, value);
    }
    command.stdin(match stdin {
        Some(file) => Stdio::from(file),
        None => Stdio::null(),
    });
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());

    verify_control_metadata(
        &request_file.metadata()?,
        request_control.identity,
        request_control.length,
    )?;
    let _ = verify_target(
        &request.executable_path,
        request.executable_identity,
        request.executable_length,
        request.target_policy,
    )?;
    if let Some(control) = &stdin_control {
        let staged_stdin = control.open(MAX_STDIN_BYTES)?;
        verify_control_metadata(&staged_stdin.metadata()?, control.identity, control.length)?;
    }
    if FileIdentity::of(&cwd.metadata()?) != request.cwd_identity {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "process broker CWD identity changed",
        ));
    }

    drop(request_file);
    let final_executable = gate.await_grant_with(|| {
        verify_target(
            &request.executable_path,
            request.executable_identity,
            request.executable_length,
            request.target_policy,
        )
    })?;
    rustix::process::fchdir(&cwd).map_err(std::io::Error::from)?;
    drop(cwd);
    write_status(&status_file, BrokerStatus::Running)?;
    drop(final_executable);
    drop(executable);
    let error = command.exec();
    let _ = write_status(&status_file, BrokerStatus::Failed);
    Err(error)
}

/// Runs the private process broker entry point without exposing request data.
#[doc(hidden)]
pub fn process_broker_main() -> ExitCode {
    match run_broker() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            let _ = std::io::stderr().write_all(b"orchestrator process broker failed\n");
            ExitCode::from(126)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_codec_preserves_non_utf8_arguments_and_environment()
    -> Result<(), Box<dyn std::error::Error>> {
        let arguments = vec![OsString::from_vec(vec![b'a', 0x80])];
        let environment = vec![(OsString::from("KEY"), OsString::from_vec(vec![0x81]))];
        let request = BrokerRequest {
            gate_challenge: GateChallenge::from_bytes([7; GATE_CHALLENGE_BYTES]),
            cwd_identity: FileIdentity {
                device: 1,
                inode: 2,
            },
            executable_path: Path::new("/sealed/executable"),
            executable_identity: FileIdentity {
                device: 3,
                inode: 4,
            },
            executable_length: 5,
            target_policy: BrokerTargetPolicy::SealedClone,
            arguments: &arguments,
            environment: &environment,
        };

        let decoded = DecodedRequest::decode(&encode_request(&request)?)?;

        assert_eq!(decoded.arguments, arguments);
        assert_eq!(decoded.environment, environment);
        assert_eq!(decoded.executable_path, Path::new("/sealed/executable"));
        Ok(())
    }

    #[test]
    fn request_codec_rejects_trailing_data() -> Result<(), Box<dyn std::error::Error>> {
        let request = BrokerRequest {
            gate_challenge: GateChallenge::from_bytes([8; GATE_CHALLENGE_BYTES]),
            cwd_identity: FileIdentity {
                device: 1,
                inode: 2,
            },
            executable_path: Path::new("/sealed/executable"),
            executable_identity: FileIdentity {
                device: 3,
                inode: 4,
            },
            executable_length: 5,
            target_policy: BrokerTargetPolicy::SealedClone,
            arguments: &[],
            environment: &[],
        };
        let mut encoded = encode_request(&request)?;
        encoded.push(0);

        assert!(DecodedRequest::decode(&encoded).is_err());
        Ok(())
    }

    #[test]
    fn request_codec_rejects_legacy_one_stage_versions() -> Result<(), Box<dyn std::error::Error>> {
        let request = BrokerRequest {
            gate_challenge: GateChallenge::from_bytes([10; GATE_CHALLENGE_BYTES]),
            cwd_identity: FileIdentity {
                device: 1,
                inode: 2,
            },
            executable_path: Path::new("/sealed/executable"),
            executable_identity: FileIdentity {
                device: 3,
                inode: 4,
            },
            executable_length: 5,
            target_policy: BrokerTargetPolicy::SealedClone,
            arguments: &[],
            environment: &[],
        };
        for stale_version in [1_u32, 2_u32] {
            let mut encoded = encode_request(&request)?;
            encoded[REQUEST_MAGIC.len()..REQUEST_MAGIC.len() + size_of::<u32>()]
                .copy_from_slice(&stale_version.to_be_bytes());
            assert!(DecodedRequest::decode(&encoded).is_err());
        }
        Ok(())
    }

    #[test]
    fn request_codec_preserves_pinned_external_target_policy()
    -> Result<(), Box<dyn std::error::Error>> {
        let identity = FileIdentity {
            device: 3,
            inode: 4,
        };
        let attestation = ExecutableFileAttestation::new(5, [9; 32]);
        let admitted_metadata = ExecutableMetadataSnapshot {
            identity,
            length: 5,
            mode: 0o100755,
            uid: 501,
            gid: 20,
            modified_seconds: 11,
            modified_nanoseconds: 12,
            changed_seconds: 13,
            changed_nanoseconds: 14,
        };
        let request = BrokerRequest {
            gate_challenge: GateChallenge::from_bytes([10; GATE_CHALLENGE_BYTES]),
            cwd_identity: FileIdentity {
                device: 1,
                inode: 2,
            },
            executable_path: Path::new("/original/executable"),
            executable_identity: identity,
            executable_length: 5,
            target_policy: BrokerTargetPolicy::PinnedExternal {
                attestation,
                admitted_metadata,
            },
            arguments: &[],
            environment: &[],
        };

        let decoded = DecodedRequest::decode(&encode_request(&request)?)?;
        let BrokerTargetPolicy::PinnedExternal {
            attestation: decoded_attestation,
            admitted_metadata: decoded_metadata,
        } = decoded.target_policy
        else {
            return Err(std::io::Error::other("external target policy was not preserved").into());
        };
        assert_eq!(decoded_attestation, attestation);
        assert!(decoded_metadata == admitted_metadata);
        assert_eq!(decoded.executable_path, Path::new("/original/executable"));
        Ok(())
    }

    #[test]
    fn request_codec_rejects_unknown_target_policy() -> Result<(), Box<dyn std::error::Error>> {
        let request = BrokerRequest {
            gate_challenge: GateChallenge::from_bytes([10; GATE_CHALLENGE_BYTES]),
            cwd_identity: FileIdentity {
                device: 1,
                inode: 2,
            },
            executable_path: Path::new("/sealed/executable"),
            executable_identity: FileIdentity {
                device: 3,
                inode: 4,
            },
            executable_length: 5,
            target_policy: BrokerTargetPolicy::SealedClone,
            arguments: &[],
            environment: &[],
        };
        let mut encoded = encode_request(&request)?;
        let policy_offset =
            REQUEST_MAGIC.len() + size_of::<u32>() + GATE_CHALLENGE_BYTES + (5 * size_of::<u64>());
        encoded[policy_offset] = u8::MAX;

        assert!(DecodedRequest::decode(&encoded).is_err());
        Ok(())
    }

    #[test]
    fn running_status_tolerates_a_staged_terminal_value_until_the_tag_commits()
    -> Result<(), Box<dyn std::error::Error>> {
        for staged in [7_i32, 15, 126] {
            let mut image = pending_status_image();
            image[STATUS_STATE_OFFSET as usize] = b'R';
            image[STATUS_VALUE_OFFSET as usize..].copy_from_slice(&staged.to_be_bytes());
            assert_eq!(decode_status(&image)?, BrokerStatus::Running);
        }

        let mut invalid_pending = pending_status_image();
        invalid_pending[STATUS_VALUE_OFFSET as usize..].copy_from_slice(&7_i32.to_be_bytes());
        assert!(decode_status(&invalid_pending).is_err());

        let mut invalid_running = pending_status_image();
        invalid_running[STATUS_STATE_OFFSET as usize] = b'R';
        invalid_running[STATUS_VALUE_OFFSET as usize..].copy_from_slice(&(-1_i32).to_be_bytes());
        assert!(decode_status(&invalid_running).is_err());
        Ok(())
    }
}
