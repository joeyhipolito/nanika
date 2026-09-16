//! CF-M3-W9. `OutboxIntent::for_mission` mints a durable effect row that
//! carries no process-request binding and therefore passes no launcher gate.
//! It is crate-private so that no out-of-crate caller — a provider process
//! included — can reach it. The argument values below are irrelevant: the
//! refusal is at the path, before anything is type-checked.

use orchestrator_app::{OutboxEffectKind, OutboxIntent};

fn main() {
    let _untyped = OutboxIntent::for_mission(
        Default::default(),
        None,
        OutboxEffectKind::GitCommand,
        Default::default(),
        1,
        Default::default(),
    );
}
