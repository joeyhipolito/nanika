fn name_root(_: Option<orchestrator_app::DurableProcessActor>) {}

fn name_nested(_: Option<orchestrator_app::durable_process_service::DurableProcessActor>) {}

fn main() {
    let _ = (name_root, name_nested);
}
