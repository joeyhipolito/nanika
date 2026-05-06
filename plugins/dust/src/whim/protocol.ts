// Canonical op-code constants for the code_diff protocol.
// Mirror the ComponentRenderer wire values so the live diff surface
// emits the same action IDs as the plugin host expects.

export const CODE_DIFF_ACCEPT_OP = 'code_diff.accept_hunk'
export const CODE_DIFF_REJECT_OP = 'code_diff.reject_hunk'
