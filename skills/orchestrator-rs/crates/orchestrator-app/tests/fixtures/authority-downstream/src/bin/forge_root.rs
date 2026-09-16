use std::path::Path;

use orchestrator_app::{CapabilityError, CapabilityRoot};

struct ForgedRoot;

impl CapabilityRoot for ForgedRoot {
    fn verify(&self) -> Result<(), CapabilityError> {
        Ok(())
    }

    fn directory(&self) -> &cap_std::fs::Dir {
        unreachable!()
    }

    fn canonical_path(&self) -> &Path {
        Path::new("/")
    }
}

fn main() {}
