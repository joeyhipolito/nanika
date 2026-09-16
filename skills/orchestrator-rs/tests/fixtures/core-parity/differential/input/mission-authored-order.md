# Authored ordering mission

PHASE: first | OBJECTIVE: Inspect inputs | PERSONA: fixture-architect
PHASE: first | OBJECTIVE: This duplicate must be ignored | PERSONA: fixture-implementer
PHASE: second | OBJECTIVE: Implement middle step | PERSONA: fixture-implementer | DEPENDS: first, third, unknown
PHASE: third | OBJECTIVE: Build final step | PERSONA: fixture-implementer | DEPENDS: second
