//! Private two-phase launcher gate.
//!
//! The launcher connects only after validating its retained launch material.
//! The parent verifies a fixed challenge and the expected direct-child PID,
//! durably records the kernel process identity outside this module, and then
//! sends an authenticated pre-start grant. The launcher acknowledges but stays
//! blocked while the parent re-observes its identity and durable composition
//! records StartedObserved. Only a second authenticated START frame permits
//! `exec`. A parent exit or dropped connection before START is fail-closed.

use std::fmt;
use std::fs::Metadata;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use super::FileIdentity;
use rustix::event::{PollFd, PollFlags, Timespec, poll};

const CHALLENGE_BYTES: usize = 32;
const HELLO_MAGIC: &[u8; 8] = b"NANGAT02";
// Version 02 is the two-stage protocol. Reusing the v1 grant would let an old
// launcher ACK and exec before the durable StartedObserved marker exists.
const GRANT_MAGIC: &[u8; 8] = b"NANGRN02";
const ACK_MAGIC: &[u8; 8] = b"NANACK02";
const START_MAGIC: &[u8; 8] = b"NANSTR01";
const START_ACK_MAGIC: &[u8; 8] = b"NANSAK01";
const HELLO_BYTES: usize = HELLO_MAGIC.len() + CHALLENGE_BYTES + size_of::<u32>();
const GRANT_BYTES: usize = GRANT_MAGIC.len() + CHALLENGE_BYTES;
const ACK_BYTES: usize = ACK_MAGIC.len() + CHALLENGE_BYTES;

/// Internal stage reached by the authenticated grant exchange.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GrantDelivery {
    /// The launcher acknowledged the complete frame.
    ///
    /// For the pre-start grant it remains blocked; for final START, the target
    /// may already have executed and this is the only proven delivery result.
    Delivered,
    /// The socket identity changed before any grant byte was written.
    NotDeliveredSocketIdentity,
    /// The grant deadline elapsed before any grant byte was written.
    NotDeliveredDeadline,
    /// Nonblocking gate I/O could not be configured before any grant byte was written.
    NotDeliveredTimeoutConfiguration,
    /// No complete grant frame was written, so launcher `read_exact` must fail closed.
    NotDeliveredIncompleteGrant,
    /// The deadline elapsed after a partial grant, so launcher `read_exact` must fail closed.
    NotDeliveredIncompleteGrantDeadline,
    /// The absolute deadline elapsed after the complete grant write but before ACK proof.
    AmbiguousAcknowledgementDeadline,
    /// A bounded ACK read could not be configured after the complete grant write.
    AmbiguousAcknowledgementReadConfiguration,
    /// A complete grant was written, but no complete acknowledgement was read.
    AmbiguousAcknowledgementRead,
    /// A complete grant was written, but the acknowledgement was not authentic.
    AmbiguousAcknowledgementMismatch,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct GateChallenge([u8; CHALLENGE_BYTES]);

impl GateChallenge {
    fn generate() -> std::io::Result<Self> {
        let mut bytes = [0_u8; CHALLENGE_BYTES];
        getrandom::fill(&mut bytes)
            .map_err(|error| std::io::Error::other(format!("launcher gate randomness: {error}")))?;
        Ok(Self(bytes))
    }

    pub(crate) const fn from_bytes(bytes: [u8; CHALLENGE_BYTES]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn as_bytes(self) -> [u8; CHALLENGE_BYTES] {
        self.0
    }
}

impl fmt::Debug for GateChallenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GateChallenge(REDACTED)")
    }
}

/// Exact private socket metadata passed to the launcher control plane.
#[derive(Clone)]
pub(crate) struct GateControl {
    path: PathBuf,
    identity: FileIdentity,
    challenge: GateChallenge,
}

impl GateControl {
    pub(crate) fn from_parts(
        path: PathBuf,
        identity: FileIdentity,
        challenge: GateChallenge,
    ) -> Self {
        Self {
            path,
            identity,
            challenge,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn identity(&self) -> FileIdentity {
        self.identity
    }

    pub(crate) fn challenge(&self) -> GateChallenge {
        self.challenge
    }
}

impl fmt::Debug for GateControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GateControl")
            .field("kind", &"private-launcher-gate")
            .finish_non_exhaustive()
    }
}

/// Parent-owned listener created before the launcher is spawned.
pub(crate) struct ParentGate {
    listener: UnixListener,
    control: GateControl,
}

impl ParentGate {
    pub(crate) fn bind(path: PathBuf) -> std::io::Result<Self> {
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(invalid_gate("launcher gate path must be absolute"));
        }
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Err(invalid_gate("launcher gate path already exists")),
            Err(error) => return Err(error),
        }

        let listener = UnixListener::bind(&path)?;
        if let Err(error) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        listener.set_nonblocking(true)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        verify_socket_metadata(&metadata, None)?;
        let control = GateControl {
            path,
            identity: FileIdentity::of(&metadata),
            challenge: GateChallenge::generate()?,
        };
        Ok(Self { listener, control })
    }

    pub(crate) fn control(&self) -> &GateControl {
        &self.control
    }

    pub(crate) fn verify(&self) -> std::io::Result<()> {
        verify_socket_path(&self.control)
    }

    /// Starts an event-driven accept/read worker.
    ///
    /// The worker waits in `poll(2)` on both the private listener and a local
    /// wake socket. It never uses a fixed sleep interval. `notify` runs exactly
    /// once with either the authenticated connection or a fail-closed error.
    pub(crate) fn spawn_waiter<F>(
        &self,
        expected_pid: u32,
        notify: F,
    ) -> std::io::Result<GateWaiter>
    where
        F: FnOnce(std::io::Result<ParentGateConnection>) + Send + 'static,
    {
        if expected_pid == 0 {
            return Err(invalid_gate("launcher gate expected PID is invalid"));
        }
        let (wake_parent, wake_worker) = UnixStream::pair()?;
        wake_parent.set_nonblocking(true)?;
        wake_worker.set_nonblocking(true)?;
        let worker_gate = Self {
            listener: self.listener.try_clone()?,
            control: self.control.clone(),
        };
        let handle = thread::Builder::new()
            .name("orchestrator-process-gate".to_owned())
            .spawn(move || notify(wait_for_launcher(worker_gate, expected_pid, &wake_worker)))?;
        Ok(GateWaiter {
            wake: wake_parent,
            handle: Some(handle),
        })
    }
}

impl fmt::Debug for ParentGate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParentGate")
            .field("kind", &"private-launcher-gate-listener")
            .finish_non_exhaustive()
    }
}

/// Abort/join ownership for the event-driven gate worker.
pub(crate) struct GateWaiter {
    wake: UnixStream,
    handle: Option<JoinHandle<()>>,
}

impl GateWaiter {
    pub(crate) fn finish(mut self) -> std::io::Result<()> {
        self.join()
    }

    pub(crate) fn abort(mut self) -> std::io::Result<()> {
        match self.wake.write(&[1]) {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error),
        }
        self.join()
    }

    fn join(&mut self) -> std::io::Result<()> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        handle
            .join()
            .map_err(|_| std::io::Error::other("launcher gate worker panicked"))
    }
}

impl Drop for GateWaiter {
    fn drop(&mut self) {
        if self.handle.is_none() {
            return;
        }
        let _ = self.wake.write(&[1]);
        let _ = self.join();
    }
}

impl fmt::Debug for GateWaiter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GateWaiter")
            .field("worker_owned", &self.handle.is_some())
            .finish()
    }
}

/// Authenticated launcher connection that has not yet received a grant.
pub(crate) struct ParentGateConnection {
    stream: UnixStream,
    control: GateControl,
}

impl ParentGateConnection {
    /// Sends the authenticated grant while retaining the final start capability.
    pub(crate) fn grant(&mut self, hard_deadline: Instant) -> GrantDelivery {
        if verify_socket_path(&self.control).is_err() {
            return GrantDelivery::NotDeliveredSocketIdentity;
        }
        exchange_grant(
            &mut self.stream,
            self.control.challenge,
            hard_deadline,
            |stream| stream.set_nonblocking(true),
        )
    }

    /// Consumes the final capability and sends the only frame that permits `exec`.
    pub(crate) fn start(mut self, hard_deadline: Instant) -> GrantDelivery {
        if verify_socket_path(&self.control).is_err() {
            return GrantDelivery::NotDeliveredSocketIdentity;
        }
        exchange_start(
            &mut self.stream,
            self.control.challenge,
            hard_deadline,
            |stream| stream.set_nonblocking(true),
        )
    }
}

fn exchange_grant<F>(
    stream: &mut UnixStream,
    challenge: GateChallenge,
    hard_deadline: Instant,
    configure_nonblocking: F,
) -> GrantDelivery
where
    F: FnMut(&UnixStream) -> std::io::Result<()>,
{
    exchange_fixed_frame(
        stream,
        grant_frame(challenge),
        ack_frame(challenge),
        hard_deadline,
        configure_nonblocking,
    )
}

fn exchange_start<F>(
    stream: &mut UnixStream,
    challenge: GateChallenge,
    hard_deadline: Instant,
    configure_nonblocking: F,
) -> GrantDelivery
where
    F: FnMut(&UnixStream) -> std::io::Result<()>,
{
    exchange_fixed_frame(
        stream,
        start_frame(challenge),
        start_ack_frame(challenge),
        hard_deadline,
        configure_nonblocking,
    )
}

fn exchange_fixed_frame<F>(
    stream: &mut UnixStream,
    frame: [u8; GRANT_BYTES],
    acknowledgement: [u8; ACK_BYTES],
    hard_deadline: Instant,
    mut configure_nonblocking: F,
) -> GrantDelivery
where
    F: FnMut(&UnixStream) -> std::io::Result<()>,
{
    if Instant::now() >= hard_deadline {
        return GrantDelivery::NotDeliveredDeadline;
    }
    if configure_nonblocking(stream).is_err() {
        return GrantDelivery::NotDeliveredTimeoutConfiguration;
    }
    if let Err(delivery) = write_grant_frame(stream, &frame, hard_deadline) {
        return delivery;
    }

    // The grant has a fixed length, so EOF is not part of its framing. On
    // Darwin a peer may send the complete ACK and close before this side
    // half-closes; `shutdown` then reports `ENOTCONN` while the ACK remains
    // readable. Only the authenticated ACK decides whether delivery is
    // proven after the complete grant write.
    read_acknowledgement_with(
        stream,
        &acknowledgement,
        hard_deadline,
        configure_nonblocking,
    )
}

#[cfg(test)]
fn read_acknowledgement(
    stream: &mut UnixStream,
    challenge: GateChallenge,
    hard_deadline: Instant,
) -> GrantDelivery {
    let acknowledgement = ack_frame(challenge);
    read_acknowledgement_with(stream, &acknowledgement, hard_deadline, |stream| {
        stream.set_nonblocking(true)
    })
}

fn read_acknowledgement_with<F>(
    stream: &mut UnixStream,
    expected: &[u8; ACK_BYTES],
    hard_deadline: Instant,
    mut configure_nonblocking: F,
) -> GrantDelivery
where
    F: FnMut(&UnixStream) -> std::io::Result<()>,
{
    if configure_nonblocking(stream).is_err() {
        return GrantDelivery::AmbiguousAcknowledgementReadConfiguration;
    }
    let mut acknowledgement = [0_u8; ACK_BYTES];
    let mut offset = 0;
    loop {
        if offset == acknowledgement.len() {
            if Instant::now() >= hard_deadline {
                return GrantDelivery::AmbiguousAcknowledgementDeadline;
            }
            return if acknowledgement == *expected {
                GrantDelivery::Delivered
            } else {
                GrantDelivery::AmbiguousAcknowledgementMismatch
            };
        }
        if Instant::now() >= hard_deadline {
            return GrantDelivery::AmbiguousAcknowledgementDeadline;
        }
        match stream.read(&mut acknowledgement[offset..]) {
            Ok(0) => return GrantDelivery::AmbiguousAcknowledgementRead,
            Ok(read) => offset += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                match wait_for_gate_io(stream, PollFlags::IN, hard_deadline) {
                    GateIoWait::Ready => {}
                    GateIoWait::Deadline => {
                        return GrantDelivery::AmbiguousAcknowledgementDeadline;
                    }
                    GateIoWait::Failed => {
                        return GrantDelivery::AmbiguousAcknowledgementRead;
                    }
                }
            }
            Err(_) => return GrantDelivery::AmbiguousAcknowledgementRead,
        }
    }
}

fn write_grant_frame(
    stream: &mut UnixStream,
    frame: &[u8; GRANT_BYTES],
    hard_deadline: Instant,
) -> Result<(), GrantDelivery> {
    write_grant_frame_with(stream, frame, hard_deadline, Instant::now, wait_for_gate_io)
}

fn write_grant_frame_with<W, N, P>(
    stream: &mut W,
    frame: &[u8; GRANT_BYTES],
    hard_deadline: Instant,
    mut now: N,
    mut wait: P,
) -> Result<(), GrantDelivery>
where
    W: Write,
    N: FnMut() -> Instant,
    P: FnMut(&W, PollFlags, Instant) -> GateIoWait,
{
    let mut offset = 0;
    loop {
        if offset == frame.len() {
            return if now() < hard_deadline {
                Ok(())
            } else {
                Err(GrantDelivery::AmbiguousAcknowledgementDeadline)
            };
        }
        if now() >= hard_deadline {
            return Err(if offset == 0 {
                GrantDelivery::NotDeliveredDeadline
            } else {
                GrantDelivery::NotDeliveredIncompleteGrantDeadline
            });
        }
        match stream.write(&frame[offset..]) {
            Ok(0) => return Err(GrantDelivery::NotDeliveredIncompleteGrant),
            Ok(written) => offset += written,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                match wait(stream, PollFlags::OUT, hard_deadline) {
                    GateIoWait::Ready => {}
                    GateIoWait::Deadline => {
                        return Err(if offset == 0 {
                            GrantDelivery::NotDeliveredDeadline
                        } else {
                            GrantDelivery::NotDeliveredIncompleteGrantDeadline
                        });
                    }
                    GateIoWait::Failed => {
                        return Err(GrantDelivery::NotDeliveredIncompleteGrant);
                    }
                }
            }
            Err(_) => return Err(GrantDelivery::NotDeliveredIncompleteGrant),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GateIoWait {
    Ready,
    Deadline,
    Failed,
}

fn wait_for_gate_io(
    stream: &UnixStream,
    interest: PollFlags,
    hard_deadline: Instant,
) -> GateIoWait {
    loop {
        let Some(remaining) = hard_deadline.checked_duration_since(Instant::now()) else {
            return GateIoWait::Deadline;
        };
        if remaining.is_zero() {
            return GateIoWait::Deadline;
        }
        let Ok(timeout) = Timespec::try_from(remaining) else {
            return GateIoWait::Failed;
        };
        let mut descriptors = [PollFd::new(stream, interest)];
        match poll(&mut descriptors, Some(&timeout)) {
            Ok(0) => return GateIoWait::Deadline,
            Ok(_) => {
                let events = descriptors[0].revents();
                if events.contains(PollFlags::NVAL) {
                    return GateIoWait::Failed;
                }
                if events.intersects(interest | PollFlags::HUP | PollFlags::ERR) {
                    return GateIoWait::Ready;
                }
            }
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return GateIoWait::Failed,
        }
    }
}

fn write_acknowledgement(
    stream: &mut UnixStream,
    acknowledgement: &[u8; ACK_BYTES],
) -> std::io::Result<()> {
    stream.write_all(acknowledgement)?;
    stream.flush()
}

impl fmt::Debug for ParentGateConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParentGateConnection")
            .field("state", &"authenticated-unreleased")
            .finish()
    }
}

/// Launcher-side connection. Dropping it before `await_grant` returns means
/// the target must not be executed.
pub(crate) struct LauncherGate {
    stream: UnixStream,
    challenge: GateChallenge,
}

impl LauncherGate {
    pub(crate) fn connect(control: &GateControl, pid: u32) -> std::io::Result<Self> {
        if pid == 0 {
            return Err(invalid_gate("launcher PID is invalid"));
        }
        verify_socket_path(control)?;
        let mut stream = UnixStream::connect(&control.path)?;
        verify_socket_path(control)?;
        stream.write_all(&hello_frame(control.challenge, pid))?;
        stream.flush()?;
        Ok(Self {
            stream,
            challenge: control.challenge,
        })
    }

    #[cfg(test)]
    pub(crate) fn await_grant(self) -> std::io::Result<()> {
        self.await_grant_with(|| Ok(()))
    }

    pub(crate) fn await_grant_with<T, F>(mut self, before_start_ack: F) -> std::io::Result<T>
    where
        F: FnOnce() -> std::io::Result<T>,
    {
        let mut frame = [0_u8; GRANT_BYTES];
        self.stream.read_exact(&mut frame)?;
        if frame != grant_frame(self.challenge) {
            return Err(invalid_gate("launcher gate grant is invalid"));
        }
        let acknowledgement = ack_frame(self.challenge);
        write_acknowledgement(&mut self.stream, &acknowledgement)?;

        // The launcher remains blocked after acknowledging the durable grant.
        // This gives the parent a stable identity to re-observe and the
        // storage actor a chance to persist its started marker. Only the
        // second authenticated frame permits the target `exec`.
        self.stream.read_exact(&mut frame)?;
        if frame != start_frame(self.challenge) {
            return Err(invalid_gate("launcher gate start is invalid"));
        }
        let result = before_start_ack()?;
        let acknowledgement = start_ack_frame(self.challenge);
        write_acknowledgement(&mut self.stream, &acknowledgement)?;
        Ok(result)
    }
}

impl fmt::Debug for LauncherGate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LauncherGate")
            .field("state", &"awaiting-grant")
            .finish()
    }
}

fn wait_for_launcher(
    gate: ParentGate,
    expected_pid: u32,
    wake: &UnixStream,
) -> std::io::Result<ParentGateConnection> {
    verify_socket_path(&gate.control)?;
    let mut stream = loop {
        wait_until_readable(&gate.listener, wake)?;
        match gate.listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error),
        }
    };
    stream.set_nonblocking(true)?;
    let mut frame = [0_u8; HELLO_BYTES];
    read_exact_wakeable(&mut stream, wake, &mut frame)?;
    if frame != hello_frame(gate.control.challenge, expected_pid) {
        return Err(invalid_gate("launcher gate hello is invalid"));
    }
    verify_socket_path(&gate.control)?;
    stream.set_nonblocking(false)?;
    Ok(ParentGateConnection {
        stream,
        control: gate.control,
    })
}

fn wait_until_readable(source: &impl std::os::fd::AsFd, wake: &UnixStream) -> std::io::Result<()> {
    loop {
        let mut descriptors = [
            PollFd::new(source, PollFlags::IN),
            PollFd::new(wake, PollFlags::IN),
        ];
        match poll(&mut descriptors, None) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(std::io::Error::from(error)),
        }
        let wake_events = descriptors[1].revents();
        if wake_events.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "launcher gate wait was cancelled",
            ));
        }
        let source_events = descriptors[0].revents();
        if source_events.contains(PollFlags::IN) {
            return Ok(());
        }
        if source_events.intersects(PollFlags::HUP | PollFlags::ERR) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "launcher gate peer closed before authentication",
            ));
        }
    }
}

fn read_exact_wakeable(
    stream: &mut UnixStream,
    wake: &UnixStream,
    mut buffer: &mut [u8],
) -> std::io::Result<()> {
    while !buffer.is_empty() {
        wait_until_readable(stream, wake)?;
        match stream.read(buffer) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "launcher gate peer closed before authentication",
                ));
            }
            Ok(read) => {
                let (_, remaining) = buffer.split_at_mut(read);
                buffer = remaining;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn hello_frame(challenge: GateChallenge, pid: u32) -> [u8; HELLO_BYTES] {
    let mut frame = [0_u8; HELLO_BYTES];
    frame[..HELLO_MAGIC.len()].copy_from_slice(HELLO_MAGIC);
    let challenge_end = HELLO_MAGIC.len() + CHALLENGE_BYTES;
    frame[HELLO_MAGIC.len()..challenge_end].copy_from_slice(&challenge.0);
    frame[challenge_end..].copy_from_slice(&pid.to_be_bytes());
    frame
}

fn grant_frame(challenge: GateChallenge) -> [u8; GRANT_BYTES] {
    let mut frame = [0_u8; GRANT_BYTES];
    frame[..GRANT_MAGIC.len()].copy_from_slice(GRANT_MAGIC);
    frame[GRANT_MAGIC.len()..].copy_from_slice(&challenge.0);
    frame
}

fn ack_frame(challenge: GateChallenge) -> [u8; ACK_BYTES] {
    let mut frame = [0_u8; ACK_BYTES];
    frame[..ACK_MAGIC.len()].copy_from_slice(ACK_MAGIC);
    frame[ACK_MAGIC.len()..].copy_from_slice(&challenge.0);
    frame
}

fn start_frame(challenge: GateChallenge) -> [u8; GRANT_BYTES] {
    let mut frame = [0_u8; GRANT_BYTES];
    frame[..START_MAGIC.len()].copy_from_slice(START_MAGIC);
    frame[START_MAGIC.len()..].copy_from_slice(&challenge.0);
    frame
}

fn start_ack_frame(challenge: GateChallenge) -> [u8; ACK_BYTES] {
    let mut frame = [0_u8; ACK_BYTES];
    frame[..START_ACK_MAGIC.len()].copy_from_slice(START_ACK_MAGIC);
    frame[START_ACK_MAGIC.len()..].copy_from_slice(&challenge.0);
    frame
}

fn verify_socket_path(control: &GateControl) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(&control.path)?;
    verify_socket_metadata(&metadata, Some(control.identity))
}

fn verify_socket_metadata(
    metadata: &Metadata,
    identity: Option<FileIdentity>,
) -> std::io::Result<()> {
    if !metadata.file_type().is_socket()
        || metadata.permissions().mode() & 0o777 != 0o600
        || identity.is_some_and(|expected| FileIdentity::of(metadata) != expected)
    {
        return Err(invalid_gate("launcher gate socket identity changed"));
    }
    Ok(())
}

fn invalid_gate(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::ErrorKind;
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Barrier};

    use super::*;

    #[cfg(target_os = "macos")]
    fn accept_darwin_peer_close(result: std::io::Result<()>) -> std::io::Result<()> {
        match result {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotConnected => Ok(()),
            Err(error) => Err(error),
        }
    }

    struct Case {
        root: PathBuf,
    }

    impl Case {
        fn create(label: &str) -> std::io::Result<Self> {
            let root = std::env::temp_dir()
                .join(format!("orchestrator-gate-{}-{label}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir(&root)?;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
            Ok(Self { root })
        }

        fn gate(&self) -> std::io::Result<ParentGate> {
            ParentGate::bind(self.root.join("gate.sock"))
        }
    }

    impl Drop for Case {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    enum WriteAction {
        Bytes(usize),
        Error(ErrorKind),
    }

    struct ScriptedWriter {
        actions: VecDeque<WriteAction>,
        bytes: Vec<u8>,
    }

    impl ScriptedWriter {
        fn new(actions: impl IntoIterator<Item = WriteAction>) -> Self {
            Self {
                actions: actions.into_iter().collect(),
                bytes: Vec::new(),
            }
        }
    }

    impl Write for ScriptedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            match self.actions.pop_front() {
                Some(WriteAction::Bytes(limit)) => {
                    let written = bytes.len().min(limit);
                    self.bytes.extend_from_slice(&bytes[..written]);
                    Ok(written)
                }
                Some(WriteAction::Error(kind)) => {
                    Err(std::io::Error::new(kind, "injected grant write"))
                }
                None => Err(std::io::Error::other("grant write script exhausted")),
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn scripted_grant_write(
        actions: impl IntoIterator<Item = WriteAction>,
        times: impl IntoIterator<Item = Instant>,
        waits: impl IntoIterator<Item = GateIoWait>,
        fallback_time: Instant,
        hard_deadline: Instant,
    ) -> (Result<(), GrantDelivery>, Vec<u8>) {
        let mut writer = ScriptedWriter::new(actions);
        let mut times: VecDeque<_> = times.into_iter().collect();
        let mut waits: VecDeque<_> = waits.into_iter().collect();
        let frame = grant_frame(GateChallenge::from_bytes([23; CHALLENGE_BYTES]));
        let result = write_grant_frame_with(
            &mut writer,
            &frame,
            hard_deadline,
            || times.pop_front().unwrap_or(fallback_time),
            |_, _, _| waits.pop_front().unwrap_or(GateIoWait::Failed),
        );
        (result, writer.bytes)
    }

    #[test]
    fn grant_write_state_machine_classifies_injected_io_exactly() {
        let started = Instant::now();
        let deadline = started + std::time::Duration::from_secs(1);
        let full_length = GRANT_BYTES;

        assert_eq!(
            scripted_grant_write([], [deadline], [], deadline, deadline).0,
            Err(GrantDelivery::NotDeliveredDeadline)
        );
        assert_eq!(
            scripted_grant_write([WriteAction::Bytes(0)], [started], [], started, deadline,).0,
            Err(GrantDelivery::NotDeliveredIncompleteGrant)
        );
        assert_eq!(
            scripted_grant_write(
                [WriteAction::Bytes(3)],
                [started, deadline],
                [],
                deadline,
                deadline,
            )
            .0,
            Err(GrantDelivery::NotDeliveredIncompleteGrantDeadline)
        );
        assert_eq!(
            scripted_grant_write(
                [
                    WriteAction::Bytes(3),
                    WriteAction::Error(ErrorKind::BrokenPipe),
                ],
                [started, started],
                [],
                started,
                deadline,
            )
            .0,
            Err(GrantDelivery::NotDeliveredIncompleteGrant)
        );
        assert_eq!(
            scripted_grant_write(
                [WriteAction::Error(ErrorKind::WouldBlock)],
                [started],
                [GateIoWait::Deadline],
                started,
                deadline,
            )
            .0,
            Err(GrantDelivery::NotDeliveredDeadline)
        );
        assert_eq!(
            scripted_grant_write(
                [
                    WriteAction::Bytes(3),
                    WriteAction::Error(ErrorKind::WouldBlock),
                ],
                [started, started],
                [GateIoWait::Deadline],
                started,
                deadline,
            )
            .0,
            Err(GrantDelivery::NotDeliveredIncompleteGrantDeadline)
        );
        assert_eq!(
            scripted_grant_write(
                [WriteAction::Error(ErrorKind::Interrupted)],
                [started, deadline],
                [],
                deadline,
                deadline,
            )
            .0,
            Err(GrantDelivery::NotDeliveredDeadline)
        );
        assert_eq!(
            scripted_grant_write(
                [WriteAction::Error(ErrorKind::WouldBlock)],
                [started, deadline],
                [GateIoWait::Ready],
                deadline,
                deadline,
            )
            .0,
            Err(GrantDelivery::NotDeliveredDeadline)
        );
        let (completed, bytes) = scripted_grant_write(
            [
                WriteAction::Error(ErrorKind::Interrupted),
                WriteAction::Error(ErrorKind::WouldBlock),
                WriteAction::Bytes(full_length),
            ],
            [started, started, started, started],
            [GateIoWait::Ready],
            started,
            deadline,
        );
        assert_eq!(completed, Ok(()));
        assert_eq!(bytes.len(), full_length);
        assert_eq!(
            scripted_grant_write(
                [WriteAction::Bytes(full_length)],
                [started, deadline],
                [],
                deadline,
                deadline,
            )
            .0,
            Err(GrantDelivery::AmbiguousAcknowledgementDeadline)
        );
    }

    #[test]
    fn nonblocking_configuration_failure_preserves_release_boundary()
    -> Result<(), Box<dyn std::error::Error>> {
        let challenge = GateChallenge::from_bytes([24; CHALLENGE_BYTES]);
        let (mut pre_grant, _pre_grant_peer) = UnixStream::pair()?;
        let mut pre_grant_calls = 0;
        let pre_grant_delivery = exchange_grant(
            &mut pre_grant,
            challenge,
            Instant::now() + std::time::Duration::from_secs(1),
            |_| {
                pre_grant_calls += 1;
                Err(std::io::Error::other(
                    "injected pre-grant configuration failure",
                ))
            },
        );
        assert_eq!(
            pre_grant_delivery,
            GrantDelivery::NotDeliveredTimeoutConfiguration
        );
        assert_eq!(pre_grant_calls, 1);

        let (mut post_grant, _post_grant_peer) = UnixStream::pair()?;
        let mut post_grant_calls = 0;
        let post_grant_delivery = exchange_grant(
            &mut post_grant,
            challenge,
            Instant::now() + std::time::Duration::from_secs(1),
            |_| {
                post_grant_calls += 1;
                if post_grant_calls == 1 {
                    Ok(())
                } else {
                    Err(std::io::Error::other(
                        "injected post-grant configuration failure",
                    ))
                }
            },
        );
        assert_eq!(
            post_grant_delivery,
            GrantDelivery::AmbiguousAcknowledgementReadConfiguration
        );
        assert_eq!(post_grant_calls, 2);
        Ok(())
    }

    #[test]
    fn grant_requires_authenticated_expected_launcher() -> Result<(), Box<dyn std::error::Error>> {
        let case = Case::create("grant")?;
        let gate = case.gate()?;
        let control = GateControl {
            path: gate.control.path.clone(),
            identity: gate.control.identity,
            challenge: gate.control.challenge,
        };
        let pid = 41_u32;
        let (sender, receiver) = channel();
        let waiter = gate.spawn_waiter(pid, move |result| {
            let _ = sender.send(result);
        })?;
        let (finished_sender, finished_receiver) = channel();
        let launcher = thread::spawn(move || {
            let result = LauncherGate::connect(&control, pid)?.await_grant();
            let _ = finished_sender.send(());
            result
        });

        let mut connection = receiver.recv()?.map_err(|error| error.to_string())?;
        assert!(format!("{connection:?}").contains("unreleased"));
        assert!(matches!(
            connection.grant(Instant::now() + std::time::Duration::from_secs(1)),
            GrantDelivery::Delivered
        ));
        assert!(matches!(
            finished_receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert!(matches!(
            connection.start(Instant::now() + std::time::Duration::from_secs(1)),
            GrantDelivery::Delivered
        ));
        launcher
            .join()
            .map_err(|_| std::io::Error::other("launcher panicked"))??;
        waiter.finish()?;
        Ok(())
    }

    #[test]
    fn wrong_pid_is_rejected_without_a_grant() -> Result<(), Box<dyn std::error::Error>> {
        let case = Case::create("wrong-pid")?;
        let gate = case.gate()?;
        let control = GateControl {
            path: gate.control.path.clone(),
            identity: gate.control.identity,
            challenge: gate.control.challenge,
        };
        let (sender, receiver) = channel();
        let waiter = gate.spawn_waiter(77, move |result| {
            let _ = sender.send(result);
        })?;
        let launcher = thread::spawn(move || LauncherGate::connect(&control, 78)?.await_grant());

        assert!(receiver.recv()?.is_err());
        assert!(
            launcher
                .join()
                .map_err(|_| std::io::Error::other("launcher panicked"))?
                .is_err()
        );
        waiter.finish()?;
        Ok(())
    }

    #[test]
    fn dropped_parent_connection_is_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
        let case = Case::create("parent-eof")?;
        let gate = case.gate()?;
        let control = GateControl {
            path: gate.control.path.clone(),
            identity: gate.control.identity,
            challenge: gate.control.challenge,
        };
        let (sender, receiver) = channel();
        let waiter = gate.spawn_waiter(91, move |result| {
            let _ = sender.send(result);
        })?;
        let launcher = thread::spawn(move || LauncherGate::connect(&control, 91)?.await_grant());

        drop(receiver.recv()?.map_err(|error| error.to_string())?);
        assert!(
            launcher
                .join()
                .map_err(|_| std::io::Error::other("launcher panicked"))?
                .is_err()
        );
        waiter.finish()?;
        Ok(())
    }

    #[test]
    fn waiter_abort_wakes_without_a_connection() -> Result<(), Box<dyn std::error::Error>> {
        let case = Case::create("abort")?;
        let gate = case.gate()?;
        let (sender, receiver) = channel();
        let waiter = gate.spawn_waiter(101, move |result| {
            let _ = sender.send(result);
        })?;

        waiter.abort()?;
        let Err(error) = receiver.recv()? else {
            return Err(
                std::io::Error::other("abort unexpectedly authenticated a launcher").into(),
            );
        };
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        Ok(())
    }

    #[test]
    fn blocked_partial_hello_can_be_aborted() -> Result<(), Box<dyn std::error::Error>> {
        let case = Case::create("partial")?;
        let gate = case.gate()?;
        let path = gate.control.path.clone();
        let (sender, receiver) = channel();
        let waiter = gate.spawn_waiter(111, move |result| {
            let _ = sender.send(result);
        })?;
        let connected = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let peer_connected = Arc::clone(&connected);
        let peer_release = Arc::clone(&release);
        let peer = thread::spawn(move || -> std::io::Result<()> {
            let mut stream = UnixStream::connect(path)?;
            stream.write_all(b"partial")?;
            peer_connected.wait();
            peer_release.wait();
            Ok(())
        });
        connected.wait();

        waiter.abort()?;
        let Err(error) = receiver.recv()? else {
            return Err(std::io::Error::other("partial hello unexpectedly authenticated").into());
        };
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        release.wait();
        peer.join()
            .map_err(|_| std::io::Error::other("peer panicked"))??;
        Ok(())
    }

    #[test]
    fn socket_replacement_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let case = Case::create("replacement")?;
        let gate = case.gate()?;
        let path = gate.control.path.clone();
        std::fs::remove_file(&path)?;
        let replacement = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        drop(replacement);
        let (sender, receiver) = channel();
        let waiter = gate.spawn_waiter(121, move |result| {
            let _ = sender.send(result);
        })?;

        assert!(receiver.recv()?.is_err());
        waiter.finish()?;
        Ok(())
    }

    #[test]
    fn replacement_before_first_grant_byte_is_not_delivered()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = Case::create("grant-prewrite")?;
        let gate = case.gate()?;
        let control = gate.control.clone();
        let path = control.path.clone();
        let (sender, receiver) = channel();
        let waiter = gate.spawn_waiter(131, move |result| {
            let _ = sender.send(result);
        })?;
        let launcher = thread::spawn(move || LauncherGate::connect(&control, 131)?.await_grant());
        let mut connection = receiver.recv()?.map_err(|error| error.to_string())?;
        waiter.finish()?;
        std::fs::remove_file(&path)?;
        let replacement = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;

        assert!(matches!(
            connection.grant(Instant::now() + std::time::Duration::from_secs(1)),
            GrantDelivery::NotDeliveredSocketIdentity
        ));
        drop(connection);
        drop(replacement);
        assert!(
            launcher
                .join()
                .map_err(|_| std::io::Error::other("launcher panicked"))?
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn complete_grant_without_acknowledgement_is_ambiguous()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = Case::create("grant-no-ack")?;
        let gate = case.gate()?;
        let control = gate.control.clone();
        let (sender, receiver) = channel();
        let waiter = gate.spawn_waiter(141, move |result| {
            let _ = sender.send(result);
        })?;
        let launcher = thread::spawn(move || -> std::io::Result<()> {
            let mut launcher = LauncherGate::connect(&control, 141)?;
            let mut frame = [0_u8; GRANT_BYTES];
            launcher.stream.read_exact(&mut frame)?;
            if frame != grant_frame(launcher.challenge) {
                return Err(invalid_gate("test launcher received invalid grant"));
            }
            Ok(())
        });
        let mut connection = receiver.recv()?.map_err(|error| error.to_string())?;
        waiter.finish()?;

        assert!(matches!(
            connection.grant(Instant::now() + std::time::Duration::from_secs(1)),
            GrantDelivery::AmbiguousAcknowledgementRead
        ));
        launcher
            .join()
            .map_err(|_| std::io::Error::other("launcher panicked"))??;
        Ok(())
    }

    #[test]
    fn new_parent_rejects_stale_v1_launcher_before_any_grant()
    -> Result<(), Box<dyn std::error::Error>> {
        const LEGACY_HELLO_MAGIC: &[u8; 8] = b"NANGAT01";
        const LEGACY_GRANT_MAGIC: &[u8; 8] = b"NANGRN01";

        let case = Case::create("stale-launcher")?;
        let gate = case.gate()?;
        let control = gate.control.clone();
        let (sender, receiver) = channel();
        let waiter = gate.spawn_waiter(146, move |result| {
            let _ = sender.send(result);
        })?;
        let (executed_sender, executed_receiver) = channel();
        let launcher = thread::spawn(move || -> std::io::Result<()> {
            let mut stream = UnixStream::connect(&control.path)?;
            let mut legacy_hello = [0_u8; HELLO_BYTES];
            legacy_hello[..LEGACY_HELLO_MAGIC.len()].copy_from_slice(LEGACY_HELLO_MAGIC);
            let challenge_end = LEGACY_HELLO_MAGIC.len() + CHALLENGE_BYTES;
            legacy_hello[LEGACY_HELLO_MAGIC.len()..challenge_end]
                .copy_from_slice(&control.challenge.0);
            legacy_hello[challenge_end..].copy_from_slice(&146_u32.to_be_bytes());
            stream.write_all(&legacy_hello)?;
            stream.flush()?;
            let mut frame = [0_u8; GRANT_BYTES];
            stream.read_exact(&mut frame)?;
            let mut legacy_grant = [0_u8; GRANT_BYTES];
            legacy_grant[..LEGACY_GRANT_MAGIC.len()].copy_from_slice(LEGACY_GRANT_MAGIC);
            legacy_grant[LEGACY_GRANT_MAGIC.len()..].copy_from_slice(&control.challenge.0);
            if frame != legacy_grant {
                return Err(invalid_gate("stale launcher received invalid legacy grant"));
            }
            let _ = executed_sender.send(());
            Ok(())
        });

        assert!(receiver.recv()?.is_err());
        waiter.finish()?;
        assert!(
            executed_receiver
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err()
        );
        assert!(
            launcher
                .join()
                .map_err(|_| std::io::Error::other("launcher panicked"))?
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn stale_v1_parent_rejects_new_launcher_before_any_grant()
    -> Result<(), Box<dyn std::error::Error>> {
        const LEGACY_HELLO_MAGIC: &[u8; 8] = b"NANGAT01";

        let case = Case::create("stale-parent")?;
        let gate = case.gate()?;
        let control = gate.control.clone();
        let listener = gate.listener.try_clone()?;
        listener.set_nonblocking(false)?;
        let (granted_sender, granted_receiver) = channel();
        let parent = thread::spawn(move || -> std::io::Result<()> {
            let (mut stream, _) = listener.accept()?;
            let mut frame = [0_u8; HELLO_BYTES];
            stream.read_exact(&mut frame)?;
            let mut legacy_hello = [0_u8; HELLO_BYTES];
            legacy_hello[..LEGACY_HELLO_MAGIC.len()].copy_from_slice(LEGACY_HELLO_MAGIC);
            let challenge_end = LEGACY_HELLO_MAGIC.len() + CHALLENGE_BYTES;
            legacy_hello[LEGACY_HELLO_MAGIC.len()..challenge_end]
                .copy_from_slice(&control.challenge.0);
            legacy_hello[challenge_end..].copy_from_slice(&147_u32.to_be_bytes());
            if frame != legacy_hello {
                return Err(invalid_gate("stale parent rejected v2 hello"));
            }
            let _ = granted_sender.send(());
            Ok(())
        });
        let launcher = LauncherGate::connect(gate.control(), 147)?;

        assert!(launcher.await_grant().is_err());
        assert!(
            granted_receiver
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err()
        );
        assert!(
            parent
                .join()
                .map_err(|_| std::io::Error::other("parent panicked"))?
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn complete_grant_with_mismatched_acknowledgement_is_ambiguous()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = Case::create("grant-wrong-ack")?;
        let gate = case.gate()?;
        let control = gate.control.clone();
        let (sender, receiver) = channel();
        let waiter = gate.spawn_waiter(151, move |result| {
            let _ = sender.send(result);
        })?;
        let launcher = thread::spawn(move || -> std::io::Result<()> {
            let mut launcher = LauncherGate::connect(&control, 151)?;
            let mut frame = [0_u8; GRANT_BYTES];
            launcher.stream.read_exact(&mut frame)?;
            if frame != grant_frame(launcher.challenge) {
                return Err(invalid_gate("test launcher received invalid grant"));
            }
            let mut acknowledgement = ack_frame(launcher.challenge);
            acknowledgement[0] ^= 0xff;
            launcher.stream.write_all(&acknowledgement)?;
            launcher.stream.flush()
        });
        let mut connection = receiver.recv()?.map_err(|error| error.to_string())?;
        waiter.finish()?;

        assert!(matches!(
            connection.grant(Instant::now() + std::time::Duration::from_secs(1)),
            GrantDelivery::AmbiguousAcknowledgementMismatch
        ));
        launcher
            .join()
            .map_err(|_| std::io::Error::other("launcher panicked"))??;
        Ok(())
    }

    #[test]
    fn complete_final_start_without_acknowledgement_is_ambiguous()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = Case::create("start-no-ack")?;
        let gate = case.gate()?;
        let control = gate.control.clone();
        let (sender, receiver) = channel();
        let waiter = gate.spawn_waiter(161, move |result| {
            let _ = sender.send(result);
        })?;
        let launcher = thread::spawn(move || -> std::io::Result<()> {
            let mut launcher = LauncherGate::connect(&control, 161)?;
            let mut frame = [0_u8; GRANT_BYTES];
            launcher.stream.read_exact(&mut frame)?;
            if frame != grant_frame(launcher.challenge) {
                return Err(invalid_gate("test launcher received invalid grant"));
            }
            let acknowledgement = ack_frame(launcher.challenge);
            write_acknowledgement(&mut launcher.stream, &acknowledgement)?;
            launcher.stream.read_exact(&mut frame)?;
            if frame != start_frame(launcher.challenge) {
                return Err(invalid_gate("test launcher received invalid start"));
            }
            Ok(())
        });
        let mut connection = receiver.recv()?.map_err(|error| error.to_string())?;
        waiter.finish()?;
        assert_eq!(
            connection.grant(Instant::now() + std::time::Duration::from_secs(1)),
            GrantDelivery::Delivered
        );

        assert_eq!(
            connection.start(Instant::now() + std::time::Duration::from_secs(1)),
            GrantDelivery::AmbiguousAcknowledgementRead
        );
        launcher
            .join()
            .map_err(|_| std::io::Error::other("launcher panicked"))??;
        Ok(())
    }

    #[test]
    fn complete_final_start_with_mismatched_acknowledgement_is_ambiguous()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = Case::create("start-wrong-ack")?;
        let gate = case.gate()?;
        let control = gate.control.clone();
        let (sender, receiver) = channel();
        let waiter = gate.spawn_waiter(171, move |result| {
            let _ = sender.send(result);
        })?;
        let launcher = thread::spawn(move || -> std::io::Result<()> {
            let mut launcher = LauncherGate::connect(&control, 171)?;
            let mut frame = [0_u8; GRANT_BYTES];
            launcher.stream.read_exact(&mut frame)?;
            if frame != grant_frame(launcher.challenge) {
                return Err(invalid_gate("test launcher received invalid grant"));
            }
            let acknowledgement = ack_frame(launcher.challenge);
            write_acknowledgement(&mut launcher.stream, &acknowledgement)?;
            launcher.stream.read_exact(&mut frame)?;
            if frame != start_frame(launcher.challenge) {
                return Err(invalid_gate("test launcher received invalid start"));
            }
            let mut acknowledgement = start_ack_frame(launcher.challenge);
            acknowledgement[0] ^= 0xff;
            write_acknowledgement(&mut launcher.stream, &acknowledgement)
        });
        let mut connection = receiver.recv()?.map_err(|error| error.to_string())?;
        waiter.finish()?;
        assert_eq!(
            connection.grant(Instant::now() + std::time::Duration::from_secs(1)),
            GrantDelivery::Delivered
        );

        assert_eq!(
            connection.start(Instant::now() + std::time::Duration::from_secs(1)),
            GrantDelivery::AmbiguousAcknowledgementMismatch
        );
        launcher
            .join()
            .map_err(|_| std::io::Error::other("launcher panicked"))??;
        Ok(())
    }

    #[test]
    fn socket_replacement_before_final_start_is_not_delivered()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = Case::create("start-replacement")?;
        let gate = case.gate()?;
        let control = gate.control.clone();
        let path = control.path.clone();
        let (sender, receiver) = channel();
        let waiter = gate.spawn_waiter(181, move |result| {
            let _ = sender.send(result);
        })?;
        let launcher = thread::spawn(move || -> std::io::Result<()> {
            let mut launcher = LauncherGate::connect(&control, 181)?;
            let mut frame = [0_u8; GRANT_BYTES];
            launcher.stream.read_exact(&mut frame)?;
            if frame != grant_frame(launcher.challenge) {
                return Err(invalid_gate("test launcher received invalid grant"));
            }
            let acknowledgement = ack_frame(launcher.challenge);
            write_acknowledgement(&mut launcher.stream, &acknowledgement)?;
            launcher.stream.read_exact(&mut frame)
        });
        let mut connection = receiver.recv()?.map_err(|error| error.to_string())?;
        waiter.finish()?;
        assert_eq!(
            connection.grant(Instant::now() + std::time::Duration::from_secs(1)),
            GrantDelivery::Delivered
        );
        std::fs::remove_file(&path)?;
        let replacement = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;

        assert_eq!(
            connection.start(Instant::now() + std::time::Duration::from_secs(1)),
            GrantDelivery::NotDeliveredSocketIdentity
        );
        drop(replacement);
        assert!(
            launcher
                .join()
                .map_err(|_| std::io::Error::other("launcher panicked"))?
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn partial_final_start_write_is_not_delivered() {
        let started = Instant::now();
        let deadline = started + std::time::Duration::from_secs(1);
        let frame = start_frame(GateChallenge::from_bytes([25; CHALLENGE_BYTES]));
        let mut writer = ScriptedWriter {
            actions: VecDeque::from([
                WriteAction::Bytes(START_MAGIC.len()),
                WriteAction::Error(std::io::ErrorKind::BrokenPipe),
            ]),
            bytes: Vec::new(),
        };

        assert_eq!(
            write_grant_frame_with(
                &mut writer,
                &frame,
                deadline,
                || started,
                |_, _, _| GateIoWait::Ready,
            ),
            Err(GrantDelivery::NotDeliveredIncompleteGrant)
        );
        assert_eq!(writer.bytes, frame[..START_MAGIC.len()]);
    }

    #[test]
    fn acknowledgement_buffered_before_peer_close_is_delivered()
    -> Result<(), Box<dyn std::error::Error>> {
        let challenge = GateChallenge::from_bytes([17; CHALLENGE_BYTES]);
        let (mut parent, mut launcher) = UnixStream::pair()?;
        let acknowledgement = ack_frame(challenge);
        write_acknowledgement(&mut launcher, &acknowledgement)?;
        drop(launcher);

        assert_eq!(
            read_acknowledgement(
                &mut parent,
                challenge,
                Instant::now() + std::time::Duration::from_secs(1),
            ),
            GrantDelivery::Delivered
        );
        Ok(())
    }

    #[test]
    fn expired_ack_budget_after_complete_grant_is_ambiguous()
    -> Result<(), Box<dyn std::error::Error>> {
        let challenge = GateChallenge::from_bytes([20; CHALLENGE_BYTES]);
        let (mut parent, _launcher) = UnixStream::pair()?;

        assert_eq!(
            read_acknowledgement(&mut parent, challenge, Instant::now()),
            GrantDelivery::AmbiguousAcknowledgementDeadline
        );
        Ok(())
    }

    #[test]
    fn partial_buffered_ack_followed_by_peer_close_is_ambiguous()
    -> Result<(), Box<dyn std::error::Error>> {
        let challenge = GateChallenge::from_bytes([21; CHALLENGE_BYTES]);
        let (mut parent, mut launcher) = UnixStream::pair()?;
        let acknowledgement = ack_frame(challenge);
        launcher.write_all(&acknowledgement[..ACK_BYTES - 1])?;
        drop(launcher);

        assert_eq!(
            read_acknowledgement(
                &mut parent,
                challenge,
                Instant::now() + std::time::Duration::from_secs(1),
            ),
            GrantDelivery::AmbiguousAcknowledgementRead
        );
        Ok(())
    }

    #[test]
    fn drip_ack_cannot_extend_the_absolute_deadline() -> Result<(), Box<dyn std::error::Error>> {
        let challenge = GateChallenge::from_bytes([22; CHALLENGE_BYTES]);
        let acknowledgement = ack_frame(challenge);
        let (mut parent, mut launcher) = UnixStream::pair()?;
        launcher.write_all(&acknowledgement[..1])?;
        let sender = thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(300));
            let _ = launcher.write_all(&acknowledgement[1..2]);
            thread::sleep(std::time::Duration::from_millis(300));
            let _ = launcher.write_all(&acknowledgement[2..]);
        });
        let started = Instant::now();
        let hard_deadline = started + std::time::Duration::from_millis(500);

        let delivery = read_acknowledgement(&mut parent, challenge, hard_deadline);

        assert_eq!(delivery, GrantDelivery::AmbiguousAcknowledgementDeadline);
        assert!(started.elapsed() < std::time::Duration::from_millis(900));
        sender
            .join()
            .map_err(|_| std::io::Error::other("drip sender panicked"))?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn buffered_ack_survives_launcher_close_regardless_of_parent_half_close()
    -> Result<(), Box<dyn std::error::Error>> {
        let challenge = GateChallenge::from_bytes([18; CHALLENGE_BYTES]);
        let (mut parent, mut launcher) = UnixStream::pair()?;
        let acknowledgement = ack_frame(challenge);
        write_acknowledgement(&mut launcher, &acknowledgement)?;
        drop(launcher);

        accept_darwin_peer_close(parent.shutdown(std::net::Shutdown::Write))?;
        assert_eq!(
            read_acknowledgement(
                &mut parent,
                challenge,
                Instant::now() + std::time::Duration::from_secs(1),
            ),
            GrantDelivery::Delivered
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn launcher_success_is_final_before_parent_close_and_half_close()
    -> Result<(), Box<dyn std::error::Error>> {
        let challenge = GateChallenge::from_bytes([19; CHALLENGE_BYTES]);
        let (mut parent, mut launcher) = UnixStream::pair()?;
        let acknowledgement = ack_frame(challenge);
        write_acknowledgement(&mut launcher, &acknowledgement)?;
        assert_eq!(
            read_acknowledgement(
                &mut parent,
                challenge,
                Instant::now() + std::time::Duration::from_secs(1),
            ),
            GrantDelivery::Delivered
        );
        drop(parent);

        accept_darwin_peer_close(launcher.shutdown(std::net::Shutdown::Write))?;
        Ok(())
    }

    #[test]
    fn debug_output_redacts_gate_material() -> Result<(), Box<dyn std::error::Error>> {
        let case = Case::create("debug")?;
        let gate = case.gate()?;
        let challenge = format!("{:?}", gate.control.challenge);
        let control = format!("{:?}", gate.control);
        let listener = format!("{gate:?}");
        assert_eq!(challenge, "GateChallenge(REDACTED)");
        assert!(!control.contains("gate.sock"));
        assert!(!listener.contains("gate.sock"));
        Ok(())
    }
}
