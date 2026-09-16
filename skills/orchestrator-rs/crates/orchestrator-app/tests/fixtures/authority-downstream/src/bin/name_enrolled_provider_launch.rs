fn name_root(_: Option<orchestrator_app::EnrolledProviderLaunch>) {}

fn name_nested(
    _: Option<
        orchestrator_app::durable_process_service::provider_process_enrollment::EnrolledProviderLaunch,
    >,
) {
}

fn main() {
    let _ = (name_root, name_nested);
}
