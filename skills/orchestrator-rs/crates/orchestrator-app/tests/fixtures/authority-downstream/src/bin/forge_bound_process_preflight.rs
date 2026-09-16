use orchestrator_exec::{BoundProcessPreflight, ProcessPreflightReason, ProcessRequest};

fn forge(request: &ProcessRequest) -> BoundProcessPreflight {
    BoundProcessPreflight {
        reason: ProcessPreflightReason::Cancelled,
        request_fingerprint: request.fingerprint(),
    }
}

fn main() {
    let _ = forge;
}
