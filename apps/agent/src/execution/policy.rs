use super::{ExecutionError, RunLimits};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionPolicy {
    default_max_turns: u32,
    max_turns: u32,
    default_max_failures_per_turn: u32,
    max_failures_per_turn: u32,
    lease_duration_seconds: i32,
    reaper_interval: std::time::Duration,
    max_supported_revisions: usize,
    max_message_batch: usize,
    reaper_batch_size: i64,
    abort_grace_seconds: i32,
}

impl ExecutionPolicy {
    pub fn new(
        default_max_turns: u32,
        max_turns: u32,
        default_max_failures_per_turn: u32,
        max_failures_per_turn: u32,
    ) -> Result<Self, ExecutionPolicyError> {
        for (name, value) in [
            ("default_max_turns", default_max_turns),
            ("max_turns", max_turns),
            (
                "default_max_failures_per_turn",
                default_max_failures_per_turn,
            ),
            ("max_failures_per_turn", max_failures_per_turn),
        ] {
            if value == 0 {
                return Err(ExecutionPolicyError::NotPositive(name));
            }
        }
        if default_max_turns > max_turns {
            return Err(ExecutionPolicyError::DefaultExceedsMaximum {
                default_name: "default_max_turns",
                maximum_name: "max_turns",
            });
        }
        if default_max_failures_per_turn > max_failures_per_turn {
            return Err(ExecutionPolicyError::DefaultExceedsMaximum {
                default_name: "default_max_failures_per_turn",
                maximum_name: "max_failures_per_turn",
            });
        }
        Ok(Self {
            default_max_turns,
            max_turns,
            default_max_failures_per_turn,
            max_failures_per_turn,
            ..Self::default()
        })
    }

    pub fn with_worker_settings(
        mut self,
        lease_duration_seconds: u32,
        reaper_interval_seconds: u64,
        max_supported_revisions: usize,
        max_message_batch: usize,
        reaper_batch_size: u32,
    ) -> Result<Self, ExecutionPolicyError> {
        for (name, positive) in [
            ("lease_duration_seconds", lease_duration_seconds > 0),
            ("reaper_interval_seconds", reaper_interval_seconds > 0),
            ("max_supported_revisions", max_supported_revisions > 0),
            ("max_message_batch", max_message_batch > 0),
            ("reaper_batch_size", reaper_batch_size > 0),
        ] {
            if !positive {
                return Err(ExecutionPolicyError::NotPositive(name));
            }
        }
        self.lease_duration_seconds = i32::try_from(lease_duration_seconds)
            .map_err(|_| ExecutionPolicyError::OutsideSupportedRange("lease_duration_seconds"))?;
        self.reaper_interval = std::time::Duration::from_secs(reaper_interval_seconds);
        self.max_supported_revisions = max_supported_revisions;
        self.max_message_batch = max_message_batch;
        self.reaper_batch_size = i64::from(reaper_batch_size);
        Ok(self)
    }

    pub fn with_abort_grace_seconds(
        mut self,
        abort_grace_seconds: u32,
    ) -> Result<Self, ExecutionPolicyError> {
        if abort_grace_seconds == 0 {
            return Err(ExecutionPolicyError::NotPositive("abort_grace_seconds"));
        }
        self.abort_grace_seconds = i32::try_from(abort_grace_seconds)
            .map_err(|_| ExecutionPolicyError::OutsideSupportedRange("abort_grace_seconds"))?;
        Ok(self)
    }

    pub(super) const fn lease_duration_seconds(self) -> i32 {
        self.lease_duration_seconds
    }

    pub const fn reaper_interval(self) -> std::time::Duration {
        self.reaper_interval
    }

    pub(super) const fn max_supported_revisions(self) -> usize {
        self.max_supported_revisions
    }

    pub(super) const fn max_message_batch(self) -> usize {
        self.max_message_batch
    }

    pub(super) const fn reaper_batch_size(self) -> i64 {
        self.reaper_batch_size
    }

    pub(super) const fn abort_grace_seconds(self) -> i32 {
        self.abort_grace_seconds
    }

    pub(super) fn resolve(self, requested: RunLimits) -> Result<ResolvedRunLimits, ExecutionError> {
        let max_turns = requested.max_turns.unwrap_or(self.default_max_turns);
        if max_turns > self.max_turns {
            return Err(ExecutionError::RunLimitExceeded {
                field: "max_turns",
                requested: max_turns,
                maximum: self.max_turns,
            });
        }
        let max_failures_per_turn = requested
            .max_failures_per_turn
            .unwrap_or(self.default_max_failures_per_turn);
        if max_failures_per_turn > self.max_failures_per_turn {
            return Err(ExecutionError::RunLimitExceeded {
                field: "max_failures_per_turn",
                requested: max_failures_per_turn,
                maximum: self.max_failures_per_turn,
            });
        }
        Ok(ResolvedRunLimits {
            max_turns,
            max_failures_per_turn,
        })
    }
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            default_max_turns: 100,
            max_turns: 1_000,
            default_max_failures_per_turn: 3,
            max_failures_per_turn: 10,
            lease_duration_seconds: 30,
            reaper_interval: std::time::Duration::from_secs(5),
            max_supported_revisions: 256,
            max_message_batch: 100,
            reaper_batch_size: 100,
            abort_grace_seconds: 30,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ResolvedRunLimits {
    pub max_turns: u32,
    pub max_failures_per_turn: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionPolicyError {
    #[error("{0} must be greater than zero")]
    NotPositive(&'static str),
    #[error("{default_name} cannot exceed {maximum_name}")]
    DefaultExceedsMaximum {
        default_name: &'static str,
        maximum_name: &'static str,
    },
    #[error("{0} is outside the supported range")]
    OutsideSupportedRange(&'static str),
}

#[cfg(test)]
mod tests {
    use super::{ExecutionPolicy, ExecutionPolicyError};
    use crate::execution::{ExecutionError, RunLimits};

    #[test]
    fn resolves_defaults_and_enforces_maximums() {
        let policy = ExecutionPolicy::new(10, 100, 2, 5).expect("policy");
        let defaults = policy.resolve(RunLimits::default()).expect("defaults");
        assert_eq!(defaults.max_turns, 10);
        assert_eq!(defaults.max_failures_per_turn, 2);

        assert!(matches!(
            policy.resolve(RunLimits {
                max_turns: Some(101),
                max_failures_per_turn: None,
            }),
            Err(ExecutionError::RunLimitExceeded {
                field: "max_turns",
                ..
            })
        ));
    }

    #[test]
    fn rejects_invalid_policy_ranges() {
        assert!(matches!(
            ExecutionPolicy::new(0, 100, 2, 5),
            Err(ExecutionPolicyError::NotPositive("default_max_turns"))
        ));
        assert!(matches!(
            ExecutionPolicy::new(101, 100, 2, 5),
            Err(ExecutionPolicyError::DefaultExceedsMaximum { .. })
        ));
    }
}
