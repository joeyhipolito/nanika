fn inspect(home: &orchestrator_app::AuthorizedProductionRuntimeHome) {
    let _ = orchestrator_app::LegacyQuiescenceProof::inspect(home);
}

fn clone_proof(proof: orchestrator_app::LegacyQuiescenceProof) {
    let _ = proof.clone();
}

fn reuse_proof(
    home: &orchestrator_app::AuthorizedProductionRuntimeHome,
    proof: orchestrator_app::LegacyQuiescenceProof,
) {
    let _ = orchestrator_app::ProductionWriterAuthority::acquire(home, "test", proof);
    let _ = orchestrator_app::ProductionWriterAuthority::acquire(home, "test", proof);
}

fn main() {
    let _ = orchestrator_app::LegacyQuiescenceProof {
        canonical_path: std::path::PathBuf::from("/tmp/forged"),
    };
}
