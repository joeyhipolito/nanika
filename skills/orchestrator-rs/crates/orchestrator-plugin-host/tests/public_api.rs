#![cfg(unix)]

use orchestrator_plugin_host::{HostError, Limits, ProductionCatalog};

#[test]
fn default_and_reviewed_production_catalogs_fail_closed_without_side_effects() {
    for catalog in [
        ProductionCatalog::default(),
        ProductionCatalog::reviewed_b5(),
    ] {
        assert!(matches!(
            catalog.enroll("review-pending", Limits::default()),
            Err(HostError::NotEnrolled)
        ));
    }
}

#[test]
fn malformed_catalog_names_fail_before_runtime_side_effects() {
    let catalog = ProductionCatalog::reviewed_b5();
    assert!(matches!(
        catalog.enroll("../../bin/sh", Limits::default()),
        Err(HostError::InvalidEnrollment)
    ));
}
