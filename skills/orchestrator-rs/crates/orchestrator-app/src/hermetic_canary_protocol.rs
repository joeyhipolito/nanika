//! Closed stdin/stdout protocol for the disposable process canary worker.

use std::io::{self, Read, Write};

use sha2::{Digest, Sha256};
use thiserror::Error;

const CHALLENGE_PREFIX: &[u8] = b"nanika-hermetic-canary-challenge-v1\0";
const CHALLENGE_BINDING_DOMAIN: &[u8] = b"nanika-hermetic-canary-binding-v1\0";
const PROOF_DOMAIN: &[u8] = b"nanika-hermetic-canary-proof-v1\0";
const PROOF_PREFIX: &str = "nanika-hermetic-canary-proof-v1\n";
pub(crate) const CHALLENGE_BYTES: usize = 32;
const CHALLENGE_FRAME_BYTES: usize = CHALLENGE_PREFIX.len() + CHALLENGE_BYTES;

/// Failure to consume one exact canary challenge or emit its bounded proof.
#[derive(Debug, Error)]
pub enum HermeticCanaryWorkerProtocolError {
    /// Stdin did not contain exactly one supported challenge frame followed by EOF.
    #[error("hermetic canary worker received an invalid challenge frame")]
    InvalidChallenge,
    /// Reading the worker's stdin failed.
    #[error("hermetic canary worker could not read its challenge: {0}")]
    Read(#[source] io::Error),
    /// Writing the worker's stdout proof failed.
    #[error("hermetic canary worker could not write its proof: {0}")]
    Write(#[source] io::Error),
}

/// Deterministic exact-admission challenge staged into the process-request fingerprint.
///
/// The challenge binds the retained executable identity, phase-worker ID, and
/// canonical worker path. An identical terminal replay reconstructs the same
/// request fingerprint; changing one of those bound inputs changes the
/// challenge.
pub(crate) struct HermeticCanaryChallenge([u8; CHALLENGE_BYTES]);

impl std::fmt::Debug for HermeticCanaryChallenge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HermeticCanaryChallenge(REDACTED)")
    }
}

impl HermeticCanaryChallenge {
    pub(crate) fn for_launch_binding(
        executable_logical_id: &str,
        executable_length: u64,
        executable_sha256: &[u8; 32],
        worker_id: &str,
        canonical_worker_path: &[u8],
    ) -> Self {
        let mut digest = Sha256::new();
        digest.update(CHALLENGE_BINDING_DOMAIN);
        update_length_prefixed(&mut digest, executable_logical_id.as_bytes());
        digest.update(executable_length.to_be_bytes());
        digest.update(executable_sha256);
        update_length_prefixed(&mut digest, worker_id.as_bytes());
        update_length_prefixed(&mut digest, canonical_worker_path);
        Self(digest.finalize().into())
    }

    #[cfg(test)]
    pub(crate) const fn from_bytes(bytes: [u8; CHALLENGE_BYTES]) -> Self {
        Self(bytes)
    }

    pub(crate) fn request_frame(&self) -> Vec<u8> {
        let mut frame = Vec::with_capacity(CHALLENGE_FRAME_BYTES);
        frame.extend_from_slice(CHALLENGE_PREFIX);
        frame.extend_from_slice(&self.0);
        frame
    }

    pub(crate) fn expected_proof(&self, pid: u32) -> Vec<u8> {
        proof_bytes(&self.0, pid)
    }
}

fn update_length_prefixed(digest: &mut Sha256, value: &[u8]) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    digest.update(length.to_be_bytes());
    digest.update(value);
}

/// Consumes exactly one challenge frame from stdin and writes only its proof to stdout.
///
/// This worker protocol never reads or writes the filesystem. The durable parent
/// owns every filesystem mutation performed after it verifies the supervised
/// process report and the exact proof bytes.
pub fn run_hermetic_canary_worker_protocol(
    mut input: impl Read,
    mut output: impl Write,
) -> Result<(), HermeticCanaryWorkerProtocolError> {
    let mut frame = [0_u8; CHALLENGE_FRAME_BYTES];
    match input.read_exact(&mut frame) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(HermeticCanaryWorkerProtocolError::InvalidChallenge);
        }
        Err(error) => return Err(HermeticCanaryWorkerProtocolError::Read(error)),
    }
    let mut trailing = [0_u8; 1];
    match input.read(&mut trailing) {
        Ok(0) => {}
        Ok(_) => return Err(HermeticCanaryWorkerProtocolError::InvalidChallenge),
        Err(error) => return Err(HermeticCanaryWorkerProtocolError::Read(error)),
    }
    if &frame[..CHALLENGE_PREFIX.len()] != CHALLENGE_PREFIX {
        return Err(HermeticCanaryWorkerProtocolError::InvalidChallenge);
    }
    let mut challenge = [0_u8; CHALLENGE_BYTES];
    challenge.copy_from_slice(&frame[CHALLENGE_PREFIX.len()..]);
    output
        .write_all(&proof_bytes(&challenge, std::process::id()))
        .map_err(HermeticCanaryWorkerProtocolError::Write)
}

fn proof_bytes(challenge: &[u8; CHALLENGE_BYTES], pid: u32) -> Vec<u8> {
    let mut digest = Sha256::new();
    digest.update(PROOF_DOMAIN);
    digest.update(challenge);
    digest.update(pid.to_be_bytes());
    let digest: [u8; 32] = digest.finalize().into();
    format!(
        "{PROOF_PREFIX}pid:{pid}\nsha256:{}\n",
        lowercase_hex(&digest)
    )
    .into_bytes()
}

fn lowercase_hex(bytes: &[u8; 32]) -> String {
    use std::fmt::Write as _;

    let mut rendered = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(rendered, "{byte:02x}");
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_frame_emits_exact_bounded_proof() {
        let challenge = HermeticCanaryChallenge::from_bytes([0x5a; CHALLENGE_BYTES]);
        let mut output = Vec::new();

        let result =
            run_hermetic_canary_worker_protocol(challenge.request_frame().as_slice(), &mut output);

        assert!(result.is_ok(), "exact frame must be accepted: {result:?}");

        assert_eq!(output, challenge.expected_proof(std::process::id()));
    }

    #[test]
    fn exact_launch_binding_reproduces_the_same_challenge() {
        let first = HermeticCanaryChallenge::for_launch_binding(
            "nanika-canary-v1-helper-digest",
            4096,
            &[0x5a; 32],
            "canary-phase-worker",
            b"/private/canary/workers/canary-phase-worker",
        );
        let replay = HermeticCanaryChallenge::for_launch_binding(
            "nanika-canary-v1-helper-digest",
            4096,
            &[0x5a; 32],
            "canary-phase-worker",
            b"/private/canary/workers/canary-phase-worker",
        );

        assert_eq!(first.request_frame(), replay.request_frame());
    }

    #[test]
    fn changed_launch_binding_changes_the_challenge() {
        let original = HermeticCanaryChallenge::for_launch_binding(
            "nanika-canary-v1-helper-digest",
            4096,
            &[0x5a; 32],
            "canary-phase-worker",
            b"/private/canary/workers/canary-phase-worker",
        )
        .request_frame();
        let changed_helper = HermeticCanaryChallenge::for_launch_binding(
            "nanika-canary-v1-other-digest",
            4096,
            &[0x5a; 32],
            "canary-phase-worker",
            b"/private/canary/workers/canary-phase-worker",
        )
        .request_frame();
        let changed_length = HermeticCanaryChallenge::for_launch_binding(
            "nanika-canary-v1-helper-digest",
            8192,
            &[0x5a; 32],
            "canary-phase-worker",
            b"/private/canary/workers/canary-phase-worker",
        )
        .request_frame();
        let changed_hash = HermeticCanaryChallenge::for_launch_binding(
            "nanika-canary-v1-helper-digest",
            4096,
            &[0xa5; 32],
            "canary-phase-worker",
            b"/private/canary/workers/canary-phase-worker",
        )
        .request_frame();
        let changed_worker = HermeticCanaryChallenge::for_launch_binding(
            "nanika-canary-v1-helper-digest",
            4096,
            &[0x5a; 32],
            "canary-other-worker",
            b"/private/canary/workers/canary-phase-worker",
        )
        .request_frame();
        let changed_worker_path = HermeticCanaryChallenge::for_launch_binding(
            "nanika-canary-v1-helper-digest",
            4096,
            &[0x5a; 32],
            "canary-phase-worker",
            b"/private/other-root/workers/canary-phase-worker",
        )
        .request_frame();

        assert_ne!(original, changed_helper);
        assert_ne!(original, changed_length);
        assert_ne!(original, changed_hash);
        assert_ne!(original, changed_worker);
        assert_ne!(original, changed_worker_path);
    }

    #[test]
    fn malformed_frame_is_rejected_without_output() {
        let mut output = Vec::new();

        let result = run_hermetic_canary_worker_protocol(b"malformed".as_slice(), &mut output);

        assert!(matches!(
            result,
            Err(HermeticCanaryWorkerProtocolError::InvalidChallenge)
        ));
        assert!(output.is_empty());
    }

    #[test]
    fn trailing_stdin_is_rejected_without_output() {
        let challenge = HermeticCanaryChallenge::from_bytes([0xa5; CHALLENGE_BYTES]);
        let mut input = challenge.request_frame();
        input.push(0);
        let mut output = Vec::new();

        let result = run_hermetic_canary_worker_protocol(input.as_slice(), &mut output);

        assert!(matches!(
            result,
            Err(HermeticCanaryWorkerProtocolError::InvalidChallenge)
        ));
        assert!(output.is_empty());
    }
}
