use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use execution_contracts::ExecutionErrorCode;

use crate::{
    connection_registry::execution_error, sandbox_accounts::SandboxAccountError,
    sandbox_materialization::SandboxMaterializationError,
};

pub(crate) struct ApiError {
    status: StatusCode,
    error: execution_contracts::ExecutionError,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            error: execution_error(ExecutionErrorCode::InvalidRequest, message, false),
        }
    }

    pub(crate) fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub(crate) fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error: execution_error(ExecutionErrorCode::NotFound, "resource not found", false),
        }
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            error: execution_error(ExecutionErrorCode::AlreadyExists, message, false),
        }
    }

    pub(crate) fn sandbox_account(error: SandboxAccountError) -> Self {
        match error {
            SandboxAccountError::InvalidName
            | SandboxAccountError::ConfigMustBeObject
            | SandboxAccountError::InvalidApiKey
            | SandboxAccountError::DisabledDefault
            | SandboxAccountError::AccountDisabled
            | SandboxAccountError::ProviderMismatch => Self::bad(error.to_string()),
            SandboxAccountError::AccountNotFound => Self::not_found(),
            SandboxAccountError::NameConflict => Self {
                status: StatusCode::CONFLICT,
                error: execution_error(ExecutionErrorCode::AlreadyExists, error.to_string(), false),
            },
            SandboxAccountError::Database(crate::db::DbError::Contract(message))
                if message == "sandbox account owns active machines"
                    || message == "sandbox account owns snapshots" =>
            {
                Self {
                    status: StatusCode::CONFLICT,
                    error: execution_error(ExecutionErrorCode::Conflict, message, false),
                }
            }
            internal => {
                tracing::error!(error=%internal, "sandbox account operation failed");
                Self {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    error: execution_error(
                        ExecutionErrorCode::Internal,
                        "internal sandbox account error",
                        true,
                    ),
                }
            }
        }
    }

    pub(crate) fn database(error: impl std::fmt::Display) -> Self {
        tracing::error!(%error, "database error");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: execution_error(
                ExecutionErrorCode::Internal,
                "internal database error",
                true,
            ),
        }
    }

    pub(crate) fn sandbox_materialization(error: SandboxMaterializationError) -> Self {
        match error {
            SandboxMaterializationError::TemplateNotFound
            | SandboxMaterializationError::SnapshotNotFound => Self::not_found(),
            SandboxMaterializationError::Account(account) => Self::sandbox_account(account),
            SandboxMaterializationError::Provider(provider) => {
                tracing::warn!(error=%provider, "sandbox provider operation failed");
                Self {
                    status: StatusCode::BAD_GATEWAY,
                    error: execution_error(
                        ExecutionErrorCode::Disconnected,
                        provider.to_string(),
                        true,
                    ),
                }
            }
            internal => {
                tracing::error!(error=%internal, "sandbox materialization failed");
                Self {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    error: execution_error(
                        ExecutionErrorCode::Internal,
                        internal.to_string(),
                        false,
                    ),
                }
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.error)).into_response()
    }
}
