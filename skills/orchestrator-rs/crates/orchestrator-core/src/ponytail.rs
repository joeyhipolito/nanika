//! Ponytail minimal-code rule injection.
//!
//! Ponytail is an external skill that instructs the LLM to write minimal code:
//! prefer stdlib, platform features, and existing dependencies over new
//! abstractions. It is the input-side / approach-layer complement to barok's
//! output-prose compression.
//!
//! Opt-in via `NANIKA_PONYTAIL=1`. Injection point: right after the
//! `## Your Task` section in `BuildCLAUDEmd`, so the minimal-code philosophy
//! frames the worker's approach before it reads prior context.

/// The env var that, when set to `"1"`, activates ponytail injection.
pub const PONYTAIL_ENV_ENABLE: &str = "NANIKA_PONYTAIL";

/// Reports whether ponytail injection is active. Controlled solely by the
/// `NANIKA_PONYTAIL` env var.
#[must_use]
pub fn ponytail_enabled() -> bool {
    std::env::var(PONYTAIL_ENV_ENABLE).ok().as_deref() == Some("1")
}

/// Returns the `## Minimal Code Rules (ponytail)` section when ponytail is
/// enabled and `skip` is false. Returns `""` otherwise.
///
/// Ports Go's `InjectPonytail` byte-faithfully.
#[must_use]
pub fn inject_ponytail(skip: bool) -> &'static str {
    if skip || !ponytail_enabled() {
        return "";
    }
    PONYTAIL_SECTION
}

/// Returns the byte length of the ponytail rule card when enabled.
#[must_use]
pub fn ponytail_rule_card_bytes() -> usize {
    inject_ponytail(false).len()
}

const PONYTAIL_SECTION: &str = "\
## Minimal Code Rules (ponytail)\n\
\n\
You are a lazy senior developer. Lazy means efficient, not careless. \
The best code is the code never written.\n\
\n\
Before writing any code, stop at the first rung that holds:\n\
\n\
1. Does this need to be built at all? (YAGNI)\n\
2. Does it already exist in this codebase? Reuse the helper, util, or pattern that's already here.\n\
3. Does the standard library already do this? Use it.\n\
4. Does a native platform feature cover it? Use it.\n\
5. Does an already-installed dependency solve it? Use it.\n\
6. Can this be one line? Make it one line.\n\
7. Only then: write the minimum code that works.\n\
\n\
The ladder runs after you understand the problem, not instead of it: \
read the task and the code it touches, trace the real flow end to end, then climb.\n\
\n\
Rules:\n\
\n\
- No abstractions that weren't explicitly requested.\n\
- No new dependency if it can be avoided.\n\
- BUT: framework-standard tooling (e.g., jest-expo for Expo, vitest for Vite) \
is not a \"new dependency\" — it is the correct tool. If the current tool cannot \
do the job, swap to the framework-standard one instead of writing shims to force it. \
Mocking out the runtime to make a wrong tool work is the anti-pattern this rule guards against.\n\
- No boilerplate nobody asked for.\n\
- Deletion over addition. Boring over clever. Fewest files possible.\n\
- Shortest working diff wins, but only once you understand the problem. \
The smallest change in the wrong place isn't lazy, it's a second bug.\n\
- Question complex requests: \"Do you actually need X, or does Y cover it?\"\n\
- Pick the edge-case-correct option when two stdlib approaches are the same size.\n\
- Mark intentional simplifications with a `ponytail:` comment. \
If the shortcut has a known ceiling (global lock, O(n²) scan, naive heuristic), \
the comment names the ceiling and the upgrade path.\n\
\n\
Not lazy about: understanding the problem (read it fully and trace the real flow \
before picking a rung), input validation at trust boundaries, error handling \
that prevents data loss, security, accessibility, calibration real hardware needs, \
anything explicitly requested. Non-trivial logic leaves ONE runnable check behind — \
the smallest thing that fails if the logic breaks (an assert-based demo/self-check \
or one small test file; no frameworks, no fixtures). Trivial one-liners need no test.\n\
\n";
