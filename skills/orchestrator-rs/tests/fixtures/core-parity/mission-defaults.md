This prose is ignored.
PHASE: prepare | OBJECTIVE: Plan the change | PERSONA: known-planner | SKILLS: rust, ,rust | DEPENDS: prepare | UNKNOWN: retained nowhere
PHASE: build | OBJECTIVE: Implement it | DEPENDS: prepare, future, missing | WORKDIR: ~/repo | TIMEOUT: 1m30s | PRIORITY: p0
PHASE: future | OBJECTIVE: Verify it | DEPENDS: build
PHASE: build | OBJECTIVE: duplicate is rejected
PHASE: missing-objective | PERSONA: known-planner
