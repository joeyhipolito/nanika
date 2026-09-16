use orchestrator_app::RetryAuthorization;

fn main() {
    let _policy = RetryAuthorization::policy(
        "effect_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        1,
        "caller-policy",
        "caller-decision",
        "2026-07-16T00:00:00Z",
    );
    let _operator = RetryAuthorization::operator(
        "effect_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        1,
        "caller-policy",
        "caller-decision",
        "2026-07-16T00:00:00Z",
    );
}
