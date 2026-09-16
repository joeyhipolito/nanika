fn name_root(_: Option<orchestrator_app::PreparedProviderLaunch>) {}

fn name_nested(
    _: Option<orchestrator_app::durable_process_service::PreparedProviderLaunch>,
) {
}

fn main() {
    let _ = (name_root, name_nested);
}
