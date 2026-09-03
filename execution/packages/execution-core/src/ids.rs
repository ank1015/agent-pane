use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::ValidationError;

macro_rules! string_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(ValidationError::single(
                        stringify!($name),
                        "must not be empty",
                    ));
                }
                if value.contains('\0') {
                    return Err(ValidationError::single(
                        stringify!($name),
                        "must not contain a null byte",
                    ));
                }
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = ValidationError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = ValidationError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

macro_rules! generated_id {
    ($name:ident) => {
        impl $name {
            #[must_use]
            pub fn generate() -> Self {
                Self(Uuid::now_v7().to_string())
            }
        }
    };
}

string_id!(ExecutionHostId, "Stable identifier for an execution host.");
string_id!(
    SupervisorGenerationId,
    "Identifier for one supervisor lifetime on an execution host."
);
string_id!(
    ExecutionId,
    "Caller-selected logical process identifier, distinct from an operating-system PID."
);
string_id!(
    OperationId,
    "Caller-selected identifier used to deduplicate a mutating operation."
);
string_id!(
    WriteId,
    "Caller-selected identifier used to deduplicate one process-input write."
);
string_id!(RootId, "Identifier for a configured filesystem root.");
string_id!(
    FileRevision,
    "Opaque file revision used for conditional mutations."
);
string_id!(
    DirectoryCursor,
    "Opaque cursor used to continue a bounded directory listing."
);

generated_id!(ExecutionHostId);
generated_id!(SupervisorGenerationId);
generated_id!(ExecutionId);
generated_id!(OperationId);
generated_id!(WriteId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_reject_blank_values() {
        let error = ExecutionId::new("  ").expect_err("blank ID should fail");
        assert_eq!(error.issues[0].path, "ExecutionId");
    }

    #[test]
    fn identifiers_validate_during_deserialization() {
        let error = serde_json::from_str::<ExecutionId>("\"\"")
            .expect_err("blank serialized ID should fail");
        assert!(error.to_string().contains("must not be empty"));
    }

    #[test]
    fn generated_identifiers_are_distinct() {
        assert_ne!(ExecutionId::generate(), ExecutionId::generate());
    }
}
