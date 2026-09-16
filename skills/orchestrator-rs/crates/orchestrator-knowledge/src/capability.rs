//! Portable capability description (B3-DESIGN §6, Addendum §5.3).
//!
//! A capability here is deliberately *more than possession of a database path*:
//! it names a namespace, a type set, an operation set, a field mask, a
//! sensitivity ceiling, a validity window bound to a registry generation, and a
//! delegation budget. This crate holds only the description; the *mintable*
//! grants that make it actionable live in `orchestrator-app`'s `capability.rs`,
//! behind that crate's sealed `CapabilityRoot`.

use std::collections::BTreeSet;

use crate::{
    envelope::Sensitivity,
    error::KnowledgeError,
    identity::{FieldName, Namespace, TypeName},
    registry::{Operation, RegistryGeneration},
};

/// Which fields of a result may leave the gateway.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldMask {
    /// Every field of the body.
    All,
    /// Only the named fields.
    Only(BTreeSet<FieldName>),
}

impl FieldMask {
    /// Reports whether `field` survives this mask.
    #[must_use]
    pub fn admits(&self, field: &str) -> bool {
        match self {
            Self::All => true,
            Self::Only(fields) => fields.iter().any(|name| name.as_str() == field),
        }
    }
}

/// How long a capability remains usable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Validity {
    /// Registry generation the capability was minted against.
    pub minted_at_generation: RegistryGeneration,
    /// Optional wall-clock expiry, as epoch milliseconds.
    pub expires_at_epoch_millis: Option<u64>,
}

impl Validity {
    /// Binds a capability to one registry generation, with no expiry.
    #[must_use]
    pub const fn at(generation: RegistryGeneration) -> Self {
        Self {
            minted_at_generation: generation,
            expires_at_epoch_millis: None,
        }
    }

    /// Binds a capability to one registry generation and an expiry instant.
    #[must_use]
    pub const fn until(generation: RegistryGeneration, epoch_millis: u64) -> Self {
        Self {
            minted_at_generation: generation,
            expires_at_epoch_millis: Some(epoch_millis),
        }
    }
}

/// How far a capability may be re-delegated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delegation {
    /// Cannot be handed on.
    NotDelegable,
    /// May be handed on exactly once.
    Once,
    /// May be handed on `depth` more times.
    Depth(u8),
}

impl Delegation {
    /// Consumes one delegation step.
    ///
    /// # Errors
    /// Returns [`KnowledgeError::DelegationRefused`] once the budget is spent.
    pub const fn delegate(self) -> Result<Self, KnowledgeError> {
        match self {
            Self::NotDelegable | Self::Depth(0) => Err(KnowledgeError::DelegationRefused),
            Self::Once => Ok(Self::NotDelegable),
            Self::Depth(depth) => Ok(Self::Depth(depth - 1)),
        }
    }
}

/// Portable description of what one holder may do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeCapability {
    /// Namespace the capability is scoped to.
    pub namespace: Namespace,
    /// Types within that namespace the capability covers.
    pub types: BTreeSet<TypeName>,
    /// Operations the capability authorizes.
    pub operations: BTreeSet<Operation>,
    /// Fields that may leave the gateway.
    pub field_mask: FieldMask,
    /// Highest sensitivity the holder may receive.
    pub sensitivity_ceiling: Sensitivity,
    /// Registry generation and expiry binding.
    pub validity: Validity,
    /// Remaining delegation budget.
    pub delegation: Delegation,
}

impl KnowledgeCapability {
    /// Checks this capability against one requested operation.
    ///
    /// # Errors
    /// Returns [`KnowledgeError::CapabilityGenerationRetired`] when the active
    /// registry has moved on, and [`KnowledgeError::CapabilityRefused`] when
    /// the namespace, type, or operation is outside the grant.
    pub fn authorize(
        &self,
        active_generation: RegistryGeneration,
        namespace: &Namespace,
        type_name: &TypeName,
        operation: Operation,
    ) -> Result<(), KnowledgeError> {
        if self.validity.minted_at_generation != active_generation {
            return Err(KnowledgeError::CapabilityGenerationRetired {
                minted: self.validity.minted_at_generation.get(),
                active: active_generation.get(),
            });
        }
        if &self.namespace != namespace
            || !self.types.contains(type_name)
            || !self.operations.contains(&operation)
        {
            return Err(KnowledgeError::CapabilityRefused {
                namespace: namespace.clone(),
                type_name: type_name.clone(),
                operation,
            });
        }
        Ok(())
    }

    /// Checks a result's sensitivity against this capability's ceiling.
    ///
    /// Because [`Sensitivity`] has no `Unknown` variant, there is no value that
    /// skips this comparison.
    ///
    /// # Errors
    /// Returns [`KnowledgeError::SensitivityAboveCeiling`] when the result is
    /// classified above what the holder may receive.
    pub fn admit_egress(
        &self,
        namespace: &Namespace,
        type_name: &TypeName,
        sensitivity: Sensitivity,
    ) -> Result<(), KnowledgeError> {
        if sensitivity > self.sensitivity_ceiling {
            return Err(KnowledgeError::SensitivityAboveCeiling {
                namespace: namespace.clone(),
                type_name: type_name.clone(),
            });
        }
        Ok(())
    }

    /// Produces a delegated copy with one delegation step consumed.
    ///
    /// # Errors
    /// Returns [`KnowledgeError::DelegationRefused`] when the budget is spent.
    pub fn delegated(&self) -> Result<Self, KnowledgeError> {
        Ok(Self {
            delegation: self.delegation.delegate()?,
            ..self.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, KnowledgeError>;

    fn capability() -> TestResult<KnowledgeCapability> {
        Ok(KnowledgeCapability {
            namespace: Namespace::new("fixture")?,
            types: [TypeName::new("widget")?].into_iter().collect(),
            operations: [Operation::Put, Operation::Get].into_iter().collect(),
            field_mask: FieldMask::All,
            sensitivity_ceiling: Sensitivity::Internal,
            validity: Validity::at(RegistryGeneration::FIRST),
            delegation: Delegation::Once,
        })
    }

    #[test]
    fn authorize_checks_generation_namespace_type_and_operation() -> TestResult {
        let capability = capability()?;
        let namespace = Namespace::new("fixture")?;
        let widget = TypeName::new("widget")?;
        let gadget = TypeName::new("gadget")?;
        assert!(
            capability
                .authorize(
                    RegistryGeneration::FIRST,
                    &namespace,
                    &widget,
                    Operation::Put
                )
                .is_ok()
        );
        assert!(matches!(
            capability.authorize(
                RegistryGeneration::FIRST,
                &namespace,
                &gadget,
                Operation::Put
            ),
            Err(KnowledgeError::CapabilityRefused { .. })
        ));
        assert!(matches!(
            capability.authorize(
                RegistryGeneration::FIRST,
                &namespace,
                &widget,
                Operation::Traverse
            ),
            Err(KnowledgeError::CapabilityRefused { .. })
        ));
        Ok(())
    }

    #[test]
    fn egress_refuses_above_the_ceiling() -> TestResult {
        let capability = capability()?;
        let namespace = Namespace::new("fixture")?;
        let widget = TypeName::new("widget")?;
        assert!(
            capability
                .admit_egress(&namespace, &widget, Sensitivity::Public)
                .is_ok()
        );
        assert!(
            capability
                .admit_egress(&namespace, &widget, Sensitivity::Secret)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn delegation_budget_is_finite() -> TestResult {
        let once = capability()?;
        let handed_on = once.delegated()?;
        assert_eq!(handed_on.delegation, Delegation::NotDelegable);
        assert!(handed_on.delegated().is_err());
        assert!(Delegation::Depth(0).delegate().is_err());
        assert_eq!(
            Delegation::Depth(2).delegate().ok(),
            Some(Delegation::Depth(1)),
        );
        Ok(())
    }

    #[test]
    fn field_mask_limits_egress_fields() -> TestResult {
        let mask = FieldMask::Only([FieldName::new("visible")?].into_iter().collect());
        assert!(mask.admits("visible"));
        assert!(!mask.admits("hidden"));
        assert!(FieldMask::All.admits("anything"));
        Ok(())
    }
}
