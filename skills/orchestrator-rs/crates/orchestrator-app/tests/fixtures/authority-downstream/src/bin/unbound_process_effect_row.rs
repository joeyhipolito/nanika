//! CF-M3-W9. The only public constructor of `OutboxIntent` is request-bound:
//! the trailing `&ProcessRequest` is what ties the durable row to a gated
//! spawn and to a fingerprint. Omitting it does not silently degrade to the
//! untyped constructor — it does not compile.

use orchestrator_app::{OutboxEffectKind, OutboxIntent};

fn main() {
    let _unbound = OutboxIntent::for_process(
        Default::default(),
        None,
        OutboxEffectKind::ProviderProcess,
        Default::default(),
        1,
        Default::default(),
    );
}
