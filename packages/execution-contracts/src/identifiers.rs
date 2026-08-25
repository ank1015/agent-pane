use std::{fmt, str::FromStr};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::validation::ValidationError;

macro_rules! non_empty_string_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates an identifier after checking that it is not blank.
            pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(ValidationError::single(
                        stringify!($name),
                        "must not be empty",
                    ));
                }
                Ok(Self(value))
            }

            /// Returns the identifier as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consumes the identifier and returns its string value.
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

non_empty_string_id!(MachineId, "Stable identifier for a registered machine.");
non_empty_string_id!(
    EnvironmentId,
    "Stable identifier for a saved execution environment."
);
non_empty_string_id!(
    WorkspaceRootId,
    "Identifier for a configured workspace root."
);
non_empty_string_id!(
    OperationId,
    "Client-generated identifier used to deduplicate an operation."
);
non_empty_string_id!(
    ExecutionId,
    "Logical process identifier, distinct from an operating-system PID."
);
non_empty_string_id!(
    ArtifactId,
    "Identifier for persisted binary or text content."
);
non_empty_string_id!(
    PreparedMutationId,
    "Identifier for a staged workspace mutation."
);
non_empty_string_id!(
    GrantId,
    "Identifier for an authorization grant scoped to a native path."
);
non_empty_string_id!(
    ContinuationCursor,
    "Opaque cursor for continuing a bounded query."
);
non_empty_string_id!(
    FileRevision,
    "Opaque revision used for conditional filesystem mutations."
);
non_empty_string_id!(
    CapabilityId,
    "Open identifier for a versioned execution capability."
);
