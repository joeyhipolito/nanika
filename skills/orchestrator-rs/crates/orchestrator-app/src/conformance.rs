use crate::ApplicationError;
use orchestrator_core::{CompatibilityVersion, ContractId};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

const BUNDLED_CONTRACTS: &str = include_str!("../../../compatibility/contracts.yaml");
const SUPPORTED_LEDGER_SCHEMA: u32 = 1;

/// Compatibility behavior assigned by the normative ledger.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum CompatibilityClassification {
    /// Observable behavior must match.
    Exact,
    /// Readers and writers obey documented versions.
    Versioned,
    /// Rust deliberately corrects a named defect.
    IntentionalFix,
    /// Runtime behavior is removed while its disposition remains explicit.
    Retired,
}

/// Whether a compatibility surface is implemented, preserved, read-only, or omitted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ContractDisposition {
    /// Implement the behavior.
    Implement,
    /// Preserve bytes without taking ownership.
    Preserve,
    /// Permit reads but no mutation.
    ReadOnly,
    /// Do not expose the retired runtime behavior.
    Omit,
}

/// Review state of one normative compatibility contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ContractStatus {
    /// The full observable contract is not yet proved and shipped.
    Incomplete,
    /// The full observable contract has executable completion evidence.
    Complete,
}

/// One normative compatibility contract.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceContract {
    /// Stable contract identifier.
    pub id: ContractId,
    /// Whether the full contract has been completed.
    pub status: ContractStatus,
    /// Executable gate that promoted this contract, required when complete.
    pub evidence: Option<String>,
    /// What that gate observed, required when complete.
    pub evidence_note: Option<String>,
    /// The observation still owed, required while incomplete.
    pub missing_observation: Option<String>,
    /// Tracker issue owning the remaining work.
    pub issue: Option<String>,
    /// Supporting gates that do not by themselves complete the contract.
    #[serde(default)]
    pub foundation_evidence: Vec<String>,
    /// Compatibility category.
    pub classification: CompatibilityClassification,
    /// Optional explicit disposition.
    pub disposition: Option<ContractDisposition>,
    /// Owning surface area.
    pub area: String,
    /// Human-readable contract summary.
    pub summary: String,
    /// Observable requirements.
    pub requirements: Vec<String>,
    /// Go packages from the frozen baseline.
    pub packages: Vec<String>,
    /// Surface-specific writer versions, retained for fixture consumers.
    #[serde(default)]
    pub current_writer_versions: BTreeMap<String, LedgerScalar>,
}

impl ConformanceContract {
    /// Requires completion status and completion evidence to travel together.
    ///
    /// A promoted contract must name the executable gate that promoted it and
    /// what that gate observed, and must no longer claim an owed observation.
    /// An unpromoted contract must state the observation it still owes and must
    /// not bank evidence ahead of the gate that would earn it. Checking both
    /// directions is what keeps the guard from degrading into a rubber stamp: a
    /// one-sided check is satisfied by a half-finished promotion.
    ///
    /// Evidence presence is checked before missing-observation state on both
    /// sides, so a mutation that violates the pairing twice fails
    /// deterministically on its evidence rather than by accident of field order.
    fn validate_evidence_pairing(&self) -> Result<(), ApplicationError> {
        let named = |field: &Option<String>| field.as_deref().is_some_and(|v| !v.trim().is_empty());
        let evidenced = named(&self.evidence) && named(&self.evidence_note);
        let owes_observation = named(&self.missing_observation);

        match self.status {
            ContractStatus::Complete => {
                if !evidenced || owes_observation {
                    return Err(ApplicationError::UnevidencedContractCompletion(
                        self.id.to_string(),
                    ));
                }
            }
            ContractStatus::Incomplete => {
                if named(&self.evidence) || named(&self.evidence_note) || !owes_observation {
                    return Err(ApplicationError::UnclaimedContractEvidence(
                        self.id.to_string(),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Scalar values used by compatibility-version declarations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum LedgerScalar {
    /// Numeric schema or envelope version.
    Number(u32),
    /// Named versioning strategy such as `additive`.
    Text(String),
}

/// Frozen Go baseline metadata retained by conformance consumers.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// Baseline implementation path.
    pub implementation: String,
    /// Frozen Git commit.
    pub git_commit: String,
    /// Observation date.
    pub observed_on: String,
    /// Human-readable normative specification.
    pub normative_document: String,
}

/// Preservation or omission rule outside the executable contract list.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreservationRule {
    /// Compatibility category.
    pub classification: CompatibilityClassification,
    /// Required disposition.
    pub disposition: ContractDisposition,
    /// Preserved path patterns, when applicable.
    #[serde(default)]
    pub globs: Vec<String>,
    /// Normative contracts supporting the rule.
    pub contract_ids: Vec<ContractId>,
}

/// Parsed, validated compatibility fixture.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceLedger {
    /// Fixture schema version.
    pub schema_version: CompatibilityVersion,
    /// Stable ledger name.
    pub ledger_id: String,
    /// Human-readable ledger title.
    pub title: String,
    /// Frozen baseline metadata.
    pub baseline: Baseline,
    /// Ledger classification descriptions.
    pub classification_definitions: BTreeMap<String, String>,
    /// Ledger disposition descriptions.
    pub disposition_definitions: BTreeMap<String, String>,
    /// Normative contracts.
    pub contracts: Vec<ConformanceContract>,
    /// Package and command coverage maps.
    pub coverage: Coverage,
    /// Byte-preservation and runtime-omission boundaries.
    pub preservation: BTreeMap<String, PreservationRule>,
}

/// Required baseline coverage maps.
#[derive(Clone, Debug, Deserialize)]
pub struct Coverage {
    /// Frozen Go package-to-contract mapping.
    pub go_packages: BTreeMap<String, Vec<ContractId>>,
    /// CLI command-to-contract mapping.
    pub commands: BTreeMap<String, Vec<ContractId>>,
}

impl ConformanceLedger {
    /// Finds a contract by its stable identifier.
    #[must_use]
    pub fn contract(&self, id: &ContractId) -> Option<&ConformanceContract> {
        self.contracts.iter().find(|contract| &contract.id == id)
    }

    fn validate(&self) -> Result<(), ApplicationError> {
        if self.schema_version.get() != SUPPORTED_LEDGER_SCHEMA {
            return Err(ApplicationError::UnsupportedLedgerSchema {
                found: self.schema_version.get(),
                expected: SUPPORTED_LEDGER_SCHEMA,
            });
        }

        let mut ids = BTreeSet::new();
        for contract in &self.contracts {
            if !ids.insert(contract.id.clone()) {
                return Err(ApplicationError::DuplicateContract(contract.id.to_string()));
            }
            contract.validate_evidence_pairing()?;
        }

        for (owner, contract_ids) in self
            .coverage
            .go_packages
            .iter()
            .chain(self.coverage.commands.iter())
        {
            for contract_id in contract_ids {
                if !ids.contains(contract_id) {
                    return Err(ApplicationError::UndefinedContractReference {
                        owner: owner.clone(),
                        contract: contract_id.to_string(),
                    });
                }
            }
        }
        for (owner, rule) in &self.preservation {
            for contract_id in &rule.contract_ids {
                if !ids.contains(contract_id) {
                    return Err(ApplicationError::UndefinedContractReference {
                        owner: owner.clone(),
                        contract: contract_id.to_string(),
                    });
                }
            }
        }

        let accounted_ids: BTreeSet<_> = self
            .coverage
            .go_packages
            .values()
            .chain(self.coverage.commands.values())
            .flatten()
            .chain(
                self.preservation
                    .values()
                    .flat_map(|rule| rule.contract_ids.iter()),
            )
            .cloned()
            .collect();
        if let Some(contract_id) = ids.difference(&accounted_ids).next() {
            return Err(ApplicationError::UnaccountedContract(
                contract_id.to_string(),
            ));
        }
        Ok(())
    }
}

/// Loads and validates a conformance ledger fixture.
pub fn load_conformance_ledger(source: &str) -> Result<ConformanceLedger, ApplicationError> {
    let ledger: ConformanceLedger = serde_saphyr::from_str(source)?;
    ledger.validate()?;
    Ok(ledger)
}

/// Loads the normative `compatibility/contracts.yaml` embedded in this build.
pub fn load_bundled_conformance_ledger() -> Result<ConformanceLedger, ApplicationError> {
    load_conformance_ledger(BUNDLED_CONTRACTS)
}

#[cfg(test)]
mod tests {
    use super::{
        BUNDLED_CONTRACTS, ContractDisposition, ContractStatus, load_bundled_conformance_ledger,
        load_conformance_ledger,
    };
    use crate::ApplicationError;
    use orchestrator_core::ContractId;

    #[test]
    fn bundled_fixture_is_valid_and_contains_safety_contracts()
    -> Result<(), Box<dyn std::error::Error>> {
        let ledger = load_bundled_conformance_ledger()?;
        let home_contract = ContractId::new("ORC-CONFIG-HOME-001")?;
        let retired_runtime = ContractId::new("ORC-NOTIFY-RUNTIME-001")?;

        assert_eq!(ledger.schema_version.get(), 1);
        assert!(ledger.contract(&home_contract).is_some());
        assert!(ledger.contract(&retired_runtime).is_some());
        assert!(!ledger.coverage.commands.is_empty());
        assert!(!ledger.coverage.go_packages.is_empty());
        assert!(
            ledger.contracts.iter().all(|contract| {
                let named =
                    |field: &Option<String>| field.as_deref().is_some_and(|v| !v.trim().is_empty());
                match contract.status {
                    ContractStatus::Complete => {
                        named(&contract.evidence)
                            && named(&contract.evidence_note)
                            && !named(&contract.missing_observation)
                    }
                    ContractStatus::Incomplete => {
                        !named(&contract.evidence)
                            && !named(&contract.evidence_note)
                            && named(&contract.missing_observation)
                    }
                }
            }),
            "a promoted compatibility contract must name the executable gate that promoted it"
        );

        // N1 -- promotion without evidence. Flipping one word must not smuggle a
        // completion past the guard. The mutated contract violates the pairing
        // twice (no evidence, and a missing observation it should have dropped);
        // evidence is checked first, so the error is deterministic.
        let false_promotion =
            BUNDLED_CONTRACTS.replacen("status: incomplete", "status: complete", 1);
        assert!(
            matches!(
                load_conformance_ledger(&false_promotion),
                Err(ApplicationError::UnevidencedContractCompletion(id)) if id == "ORC-CLI-ROOT-001"
            ),
            "the completion guard must observe, rather than silently ignore, status promotion"
        );

        // N2 -- evidence without promotion. The opposite direction of the same
        // pairing: evidence must not be banked ahead of the gate that earns it.
        // A single negative case in either direction is satisfiable by a
        // one-sided check, which is how the blanket ban came to be.
        let unclaimed_evidence = BUNDLED_CONTRACTS.replacen(
            "    status: complete\n    evidence: refreshed_base_e2e\n",
            "    status: incomplete\n    evidence: refreshed_base_e2e\n",
            1,
        );
        assert!(
            matches!(
                load_conformance_ledger(&unclaimed_evidence),
                Err(ApplicationError::UnclaimedContractEvidence(id)) if id == "ORC-GIT-BASE-001"
            ),
            "the completion guard must refuse evidence banked ahead of a green gate"
        );
        Ok(())
    }

    #[test]
    fn every_contract_id_is_accounted_for_by_coverage_or_preservation()
    -> Result<(), Box<dyn std::error::Error>> {
        let ledger = load_bundled_conformance_ledger()?;
        let mut accounted = std::collections::BTreeSet::new();
        for ids in ledger
            .coverage
            .go_packages
            .values()
            .chain(ledger.coverage.commands.values())
        {
            accounted.extend(ids.iter().cloned());
        }
        for rule in ledger.preservation.values() {
            accounted.extend(rule.contract_ids.iter().cloned());
        }

        let contract_ids: std::collections::BTreeSet<_> = ledger
            .contracts
            .iter()
            .map(|contract| contract.id.clone())
            .collect();
        assert_eq!(accounted, contract_ids);
        assert_eq!(contract_ids.len(), 42);
        Ok(())
    }

    #[test]
    fn retired_contract_dispositions_are_explicit() -> Result<(), Box<dyn std::error::Error>> {
        let ledger = load_bundled_conformance_ledger()?;
        let expected = [
            ("ORC-NOTIFY-RUNTIME-001", ContractDisposition::Omit),
            ("ORC-NOTIFY-DATA-001", ContractDisposition::Preserve),
            (
                "ORC-LEGACY-INTEGRATION-RUNTIME-001",
                ContractDisposition::Omit,
            ),
            (
                "ORC-LEGACY-INTEGRATION-DATA-001",
                ContractDisposition::Preserve,
            ),
        ];
        for (id, disposition) in expected {
            let id = ContractId::new(id)?;
            assert_eq!(
                ledger
                    .contract(&id)
                    .and_then(|contract| contract.disposition),
                Some(disposition),
                "retired contract {id} has the wrong disposition"
            );
        }
        Ok(())
    }

    #[test]
    fn ledger_loader_rejects_invalid_contract_ledgers() {
        let unsupported = BUNDLED_CONTRACTS.replacen("schema_version: 1", "schema_version: 2", 1);
        assert!(matches!(
            load_conformance_ledger(&unsupported),
            Err(ApplicationError::UnsupportedLedgerSchema {
                found: 2,
                expected: 1
            })
        ));

        let unaccounted =
            BUNDLED_CONTRACTS.replace("    orchestrator run: [ORC-CLI-RUN-001]\n", "");
        assert!(matches!(
            load_conformance_ledger(&unaccounted),
            Err(ApplicationError::UnaccountedContract(id)) if id == "ORC-CLI-RUN-001"
        ));

        let duplicate =
            BUNDLED_CONTRACTS.replacen("  - id: ORC-CLI-RUN-001", "  - id: ORC-CLI-ROOT-001", 1);
        assert!(matches!(
            load_conformance_ledger(&duplicate),
            Err(ApplicationError::DuplicateContract(id)) if id == "ORC-CLI-ROOT-001"
        ));

        let undefined = BUNDLED_CONTRACTS.replacen(
            "    orchestrator: [ORC-CLI-ROOT-001]",
            "    orchestrator: [ORC-DOES-NOT-EXIST-001]",
            1,
        );
        assert!(matches!(
            load_conformance_ledger(&undefined),
            Err(ApplicationError::UndefinedContractReference { owner, contract })
                if owner == "orchestrator" && contract == "ORC-DOES-NOT-EXIST-001"
        ));
    }
}
