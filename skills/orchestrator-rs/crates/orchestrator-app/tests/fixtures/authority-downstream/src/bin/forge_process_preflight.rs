use orchestrator_exec::{ProcessPreflight, ProcessPreflightReason, ProcessRequest, ProcessService};

fn forge<'service>(
    service: &'service dyn ProcessService,
    request: &ProcessRequest,
) -> ProcessPreflight<'service> {
    ProcessPreflight {
        reason: ProcessPreflightReason::Cancelled,
        request_fingerprint: request.fingerprint(),
        service_identity: todo!(),
        _service_borrow: service,
    }
}

// `ProcessPreflight::new` privacy is proven by the `compile_fail` doctest on
// the type itself; naming it here would abort privacy checking before the
// struct-literal rejection below is emitted.
fn main() {
    let _ = forge;
}
