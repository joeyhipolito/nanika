use orchestrator_app::{
    AuthorizedFixtureRuntimeHome, LegacyQuiescenceProof, ProductionWriterAuthority,
};

fn promote(home: &AuthorizedFixtureRuntimeHome, proof: LegacyQuiescenceProof) {
    let _ = ProductionWriterAuthority::acquire(home, "test", proof);
}

fn main() {}
