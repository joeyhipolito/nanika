//! Versioned admitted feature settings; observed application is reported separately.

use serde::{Deserialize, Serialize};

const FEATURES: [&str; 8] = [
    "barok",
    "ponytail",
    "discipline",
    "portal-output-cap",
    "portal-review-summary",
    "kb",
    "learnings",
    "worker-memory",
];

const SCHEMA: &str = "nanika.run-features.v2";
const LEGACY_SCHEMA: &str = "nanika.run-features.v1";
const IMPLEMENTATION_REVISION: &str = "rust-pilot-features/v2";
const LEGACY_REVISION: &str = "rust-pilot-features/v1";
const NOT_WIRED_REASON: &str = "not-wired-in-rust-pilot";

fn feature_index(name: &str) -> Option<usize> {
    FEATURES.iter().position(|candidate| *candidate == name)
}

fn is_supported_runtime(runtime: &str) -> bool {
    matches!(runtime, "claude" | "codex")
}

fn is_supported_command(command: &str) -> bool {
    matches!(command, "code" | "review" | "run")
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RequestedMode {
    Off,
    On,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum EffectiveMode {
    Off,
    On,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Source {
    RuntimeDefault,
    ExplicitRunOption,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    requested: Option<RequestedMode>,
    effective: EffectiveMode,
    supported: bool,
    applied: Option<bool>,
    reason: String,
    source: Source,
}

/// Immutable receipt of a feature request evaluation for a given runtime and command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    schema: String,
    implementation_revision: String,
    runtime: String,
    command: String,
    entries: Vec<Entry>,
}

impl Snapshot {
    /// Reconstructs the [`Requests`] that would produce this exact snapshot,
    /// rejecting tampering, unknown schema/revision, or unsupported ON state.
    pub(crate) fn restore(&self) -> Result<Requests, String> {
        let legacy = match (self.schema.as_str(), self.implementation_revision.as_str()) {
            (LEGACY_SCHEMA, LEGACY_REVISION) => true,
            (SCHEMA, IMPLEMENTATION_REVISION) => false,
            _ => return Err("unknown feature schema or implementation revision".to_owned()),
        };
        if !is_supported_runtime(&self.runtime) {
            return Err(format!("unsupported runtime in snapshot: {}", self.runtime));
        }
        if !is_supported_command(&self.command) {
            return Err(format!("unsupported command in snapshot: {}", self.command));
        }
        if self.entries.len() != FEATURES.len() {
            return Err(format!(
                "expected {} feature rows, found {}",
                FEATURES.len(),
                self.entries.len()
            ));
        }

        let mut requests = Requests::default();
        for (idx, expected_name) in FEATURES.iter().enumerate() {
            let entry = &self.entries[idx];
            if entry.name != *expected_name {
                return Err(format!(
                    "feature row {idx} out of order: expected {expected_name}, found {}",
                    entry.name
                ));
            }
            match (&entry.requested, &entry.source) {
                (None, Source::RuntimeDefault) => {}
                (Some(RequestedMode::Off), Source::ExplicitRunOption) => {
                    requests.explicit_off[idx] = true;
                }
                (Some(RequestedMode::On), Source::ExplicitRunOption) if idx == 3 && !legacy => {
                    requests.portal_on = true;
                }
                _ => {
                    return Err(format!(
                        "feature {} has inconsistent requested/source pairing",
                        entry.name
                    ));
                }
            }
        }

        let canonical = requests.snapshot_version(&self.runtime, &self.command, legacy)?;
        if canonical != *self {
            return Err("feature snapshot does not match canonical reconstruction".to_string());
        }
        Ok(requests)
    }

    pub(crate) fn context_matches(&self, runtime: &str, command: &str) -> bool {
        self.runtime == runtime && self.command == command
    }

    /// Renders this snapshot as JSON. Infallible for these plain-data types.
    pub(crate) fn value(&self) -> serde_json::Value {
        serde_json::json!(self)
    }
}

/// Bounded requests. Portal output capping is admitted only for standalone Codex code.
#[derive(Default, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Requests {
    explicit_off: [bool; FEATURES.len()],
    portal_on: bool,
}

impl Requests {
    /// Records one explicit request; invalid or duplicate requests do not mutate state.
    pub(crate) fn request(&mut self, flag: &str) -> Result<(), String> {
        if flag.is_empty() {
            return Err("empty feature flag".to_string());
        }
        if !flag.is_ascii() {
            return Err(format!("non-ascii feature flag: {flag:?}"));
        }
        if flag.matches('=').count() != 1 {
            return Err(format!(
                "malformed feature flag (expected exactly one '='): {flag:?}"
            ));
        }
        let Some((name, value)) = flag.split_once('=') else {
            return Err(format!("malformed feature flag: {flag:?}"));
        };
        if name.is_empty() || value.is_empty() {
            return Err(format!("malformed feature flag: {flag:?}"));
        }
        let idx = feature_index(name).ok_or_else(|| format!("unknown feature: {name}"))?;
        match value {
            "off" => {
                if self.explicit_off[idx] || (idx == 3 && self.portal_on) {
                    return Err(format!("duplicate feature request: {name}"));
                }
                self.explicit_off[idx] = true;
                Ok(())
            }
            "on" if idx == 3 => {
                if self.explicit_off[idx] || self.portal_on {
                    return Err(format!("duplicate feature request: {name}"));
                }
                self.portal_on = true;
                Ok(())
            }
            "on" => Err(format!(
                "feature {name} does not support explicit on in the rust pilot (fails closed, not silently downgraded)"
            )),
            other => Err(format!("invalid feature value for {name}: {other:?}")),
        }
    }

    /// Produces the immutable, schema-versioned snapshot for `runtime` and
    /// `command`, listing all eight known feature ids in canonical order.
    pub(crate) fn portal_output_cap(&self) -> bool {
        self.portal_on
    }

    pub(crate) fn snapshot(&self, runtime: &str, command: &str) -> Result<Snapshot, String> {
        self.snapshot_version(runtime, command, false)
    }

    fn snapshot_version(
        &self,
        runtime: &str,
        command: &str,
        legacy: bool,
    ) -> Result<Snapshot, String> {
        if !is_supported_runtime(runtime) {
            return Err(format!("unsupported runtime: {runtime}"));
        }
        if !is_supported_command(command) {
            return Err(format!("unsupported command: {command}"));
        }

        let portal_supported = !legacy && runtime == "codex" && command == "code";
        if self.portal_on && !portal_supported {
            return Err(
                "portal-output-cap=on is supported only by standalone code --runtime codex"
                    .to_owned(),
            );
        }
        let entries = FEATURES
            .iter()
            .enumerate()
            .map(|(idx, name)| {
                let on = idx == 3 && self.portal_on;
                let explicit = self.explicit_off[idx] || on;
                let supported = idx == 3 && portal_supported;
                Entry {
                    name: (*name).to_string(),
                    requested: if on {
                        Some(RequestedMode::On)
                    } else if explicit {
                        Some(RequestedMode::Off)
                    } else {
                        None
                    },
                    effective: if on {
                        EffectiveMode::On
                    } else {
                        EffectiveMode::Off
                    },
                    supported,
                    applied: if legacy { Some(false) } else { None },
                    reason: if on {
                        "command-wrapper-configured; see portal-application.json"
                    } else if supported {
                        "disabled"
                    } else {
                        NOT_WIRED_REASON
                    }
                    .to_owned(),
                    source: if explicit {
                        Source::ExplicitRunOption
                    } else {
                        Source::RuntimeDefault
                    },
                }
            })
            .collect();

        Ok(Snapshot {
            schema: if legacy { LEGACY_SCHEMA } else { SCHEMA }.to_string(),
            implementation_revision: if legacy {
                LEGACY_REVISION
            } else {
                IMPLEMENTATION_REVISION
            }
            .to_string(),
            runtime: runtime.to_string(),
            command: command.to_string(),
            entries,
        })
    }
}

/// Read-only catalog; availability is constrained by command and runtime.
pub(crate) fn catalog() -> serde_json::Value {
    serde_json::json!({
        "schema": "nanika.feature-catalog.v1",
        "implementation_revision": IMPLEMENTATION_REVISION,
        "scope": "rust-pilot-worker",
        "note": "portal-output-cap supports standalone Codex code through an instructed wrapper with post-run validation. Other ON requests are unsupported. Configuration is distinct from observed application; see portal-application.json. Unwrapped output may reach the provider during a rejected attempt.",
        "capabilities": [{"name":"portal-output-cap", "runtime":"codex", "command":"code", "default":"off", "mechanism":"instructed-wrapper-with-post-run-validation"}],
        "features": FEATURES,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rows_are_not_fabricated_as_requested() -> Result<(), String> {
        let requests = Requests::default();
        let snapshot = requests.snapshot("claude", "run")?;
        for entry in &snapshot.entries {
            assert_eq!(entry.requested, None);
            assert_eq!(entry.source, Source::RuntimeDefault);
            assert_eq!(entry.effective, EffectiveMode::Off);
            assert!(!entry.supported);
            assert_eq!(entry.applied, None);
            assert_eq!(entry.reason, NOT_WIRED_REASON);
        }
        assert_eq!(snapshot.entries.len(), FEATURES.len());
        Ok(())
    }

    #[test]
    fn explicit_off_is_recorded() -> Result<(), String> {
        let mut requests = Requests::default();
        requests.request("kb=off")?;
        let snapshot = requests.snapshot("codex", "code")?;
        let kb_idx = feature_index("kb").ok_or("kb is a known feature")?;
        assert_eq!(snapshot.entries[kb_idx].requested, Some(RequestedMode::Off));
        assert_eq!(snapshot.entries[kb_idx].source, Source::ExplicitRunOption);
        assert_eq!(snapshot.entries[kb_idx].effective, EffectiveMode::Off);
        Ok(())
    }

    #[test]
    fn invalid_flags_are_rejected_without_mutating_state() {
        let cases = [
            "",
            "kb",
            "kb=",
            "=off",
            "kb=off=extra",
            "kb=maybe",
            "unknown-feature=off",
            "kb=on",
            "kb=of\u{e9}f",
        ];
        for flag in cases {
            let mut requests = Requests::default();
            let before = requests.clone();
            let result = requests.request(flag);
            assert!(result.is_err(), "expected {flag:?} to be rejected");
            assert_eq!(requests, before, "state mutated on rejected flag {flag:?}");
        }
    }

    #[test]
    fn duplicate_request_is_rejected_without_mutating_state() -> Result<(), String> {
        let mut requests = Requests::default();
        requests.request("barok=off")?;
        let before = requests.clone();
        let result = requests.request("barok=off");
        assert!(result.is_err());
        assert_eq!(requests, before);
        Ok(())
    }

    #[test]
    fn all_valid_runtimes_and_commands_roundtrip() -> Result<(), String> {
        for runtime in ["claude", "codex"] {
            for command in ["code", "review", "run"] {
                let mut requests = Requests::default();
                requests.request("ponytail=off")?;
                requests.request("worker-memory=off")?;
                let snapshot = requests.snapshot(runtime, command)?;
                let restored = snapshot.restore()?;
                assert_eq!(restored, requests);
            }
        }
        Ok(())
    }

    #[test]
    fn unknown_context_is_refused() {
        let requests = Requests::default();
        assert!(requests.snapshot("unknown-runtime", "run").is_err());
        assert!(requests.snapshot("claude", "unknown-command").is_err());
    }

    #[test]
    fn tampered_effective_field_is_refused() -> Result<(), String> {
        let requests = Requests::default();
        let snapshot = requests.snapshot("claude", "run")?;
        let mut value = snapshot.value();
        value["entries"][0]["effective"] = serde_json::json!("on");
        let tampered: Snapshot = serde_json::from_value(value).map_err(|e| e.to_string())?;
        assert!(tampered.restore().is_err());
        Ok(())
    }

    #[test]
    fn tampered_supported_field_is_refused() -> Result<(), String> {
        let requests = Requests::default();
        let snapshot = requests.snapshot("codex", "review")?;
        let mut value = snapshot.value();
        value["entries"][2]["supported"] = serde_json::json!(true);
        let tampered: Snapshot =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        assert!(tampered.restore().is_err());
        Ok(())
    }

    #[test]
    fn missing_row_is_refused() -> Result<(), String> {
        let requests = Requests::default();
        let snapshot = requests.snapshot("claude", "run")?;
        let mut value = snapshot.value();
        value["entries"]
            .as_array_mut()
            .ok_or("entries is an array")?
            .pop();
        let tampered: Snapshot =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        assert!(tampered.restore().is_err());
        Ok(())
    }

    #[test]
    fn duplicate_row_is_refused() -> Result<(), String> {
        let requests = Requests::default();
        let snapshot = requests.snapshot("claude", "run")?;
        let mut value = snapshot.value();
        let entries = value["entries"]
            .as_array_mut()
            .ok_or("entries is an array")?;
        let first = entries[0].clone();
        entries[1] = first;
        let tampered: Snapshot =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        assert!(tampered.restore().is_err());
        Ok(())
    }

    #[test]
    fn unknown_revision_fails_closed() -> Result<(), String> {
        let requests = Requests::default();
        let snapshot = requests.snapshot("claude", "run")?;
        let mut value = snapshot.value();
        value["implementation_revision"] = serde_json::json!("rust-pilot-features/v99");
        let tampered: Snapshot =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        assert!(tampered.restore().is_err());
        Ok(())
    }

    #[test]
    fn unknown_schema_fails_closed() -> Result<(), String> {
        let requests = Requests::default();
        let snapshot = requests.snapshot("claude", "run")?;
        let mut value = snapshot.value();
        value["schema"] = serde_json::json!("nanika.run-features.v99");
        let tampered: Snapshot =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        assert!(tampered.restore().is_err());
        Ok(())
    }

    #[test]
    fn catalog_lists_all_features_and_is_closed_scope() -> Result<(), String> {
        let value = catalog();
        assert_eq!(
            value["schema"],
            serde_json::json!("nanika.feature-catalog.v1")
        );
        assert_eq!(value["scope"], serde_json::json!("rust-pilot-worker"));
        let features = value["features"].as_array().ok_or("features is an array")?;
        assert_eq!(features.len(), FEATURES.len());
        Ok(())
    }
}

#[cfg(test)]
mod portal_tests {
    use super::*;
    #[test]
    fn portal_on_is_context_bounded_and_application_remains_unobserved() -> Result<(), String> {
        let mut request = Requests::default();
        request.request("portal-output-cap=on")?;
        for (runtime, command) in [("claude", "code"), ("codex", "run"), ("codex", "review")] {
            assert!(request.snapshot(runtime, command).is_err());
        }
        let snapshot = request.snapshot("codex", "code")?;
        assert_eq!(snapshot.entries[3].effective, EffectiveMode::On);
        assert_eq!(snapshot.entries[3].applied, None);
        assert!(snapshot.entries[3].supported);
        assert_eq!(snapshot.restore()?, request);
        assert!(request.request("portal-output-cap=off").is_err());
        Ok(())
    }
    #[test]
    fn legacy_v1_restores_against_frozen_capabilities() -> Result<(), String> {
        let mut request = Requests::default();
        request.request("portal-output-cap=off")?;
        let legacy = request.snapshot_version("codex", "code", true)?;
        assert_eq!(legacy.schema, "nanika.run-features.v1");
        assert!(!legacy.entries[3].supported);
        assert_eq!(legacy.entries[3].applied, Some(false));
        let serialized = serde_json::to_vec(&legacy).map_err(|e| e.to_string())?;
        let restored: Snapshot = serde_json::from_slice(&serialized).map_err(|e| e.to_string())?;
        assert_eq!(restored.restore()?, request);
        assert_eq!(restored, legacy);
        let current = request.snapshot("codex", "code")?;
        assert!(current.entries[3].supported);
        assert_eq!(current.entries[3].applied, None);
        Ok(())
    }
}
