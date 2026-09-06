use std::{str, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{
        HeaderMap, HeaderValue, Request, StatusCode,
        header::{AUTHORIZATION, CACHE_CONTROL},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use execution_api::{
    ClaimRegisteredHostRequest, ClaimRegisteredHostResponse, CreateE2bAccountRequest,
    CreateExecutionHostRequest, CreateRegisteredHostRequest, CreateSnapshotRequest,
    E2bAccountStatus, E2bHostSource, ExecutionHostKind, ExecutionHostState, GatewayHostMessage,
    HostDaemonMessage, RegisteredHostRegistration, RenewRegisteredHostTokenResponse,
    ReplaceE2bCredentialRequest, UpdateE2bAccountRequest, UpdateExecutionHostRequest,
    UpdateSnapshotRequest, VersionResponse,
};
use execution_core::{ExecutionHostId, Validate};
use execution_e2b::{
    DEFAULT_EXECUTION_BASE_RAM_MB, E2bApiKey, E2bErrorKind, EXECUTION_BASE_RAM_OPTIONS_MB,
};
use execution_wire::{PROTOCOL_NAME, PROTOCOL_VERSION, RequestEnvelope, ResponseEnvelope};
use rand::{RngCore, rngs::OsRng};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{
    crypto::CredentialVault,
    database::{Database, HostFailure, HostListFilter, SnapshotListFilter},
    error::{GatewayError, provider_http_error},
    host_connections::{ConnectionOutput, HostConnectionError, HostConnections},
    lifecycle::{LifecycleReconciler, credential_context},
    provider::DynE2bProvider,
    security::SecurityControls,
};

#[derive(Clone)]
pub struct AppState {
    pub database: Database,
    pub vault: CredentialVault,
    pub provider: DynE2bProvider,
    pub reconciler: LifecycleReconciler,
    pub api_token: Arc<str>,
    pub default_host_timeout_seconds: u64,
    pub public_base_url: url::Url,
    pub registration_token_ttl: Duration,
    pub heartbeat_interval: Duration,
    pub connections: HostConnections,
    pub security: SecurityControls,
}

pub fn router(state: AppState, max_request_bytes: usize) -> Router {
    let protected_health = Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(readiness))
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize));

    let protected_control = Router::new()
        .route("/version", get(version))
        .route("/e2b-accounts", post(create_account).get(list_accounts))
        .route(
            "/e2b-accounts/{account_id}",
            get(get_account)
                .patch(update_account)
                .delete(delete_account),
        )
        .route(
            "/e2b-accounts/{account_id}/credential",
            put(replace_account_credential),
        )
        .route("/e2b-accounts/{account_id}/verify", post(verify_account))
        .route("/registered-hosts", post(create_registered_host))
        .route(
            "/registered-hosts/{host_id}/registration-token",
            post(renew_registered_host_token),
        )
        .route("/hosts", post(create_host).get(list_hosts))
        .route(
            "/hosts/{host_id}",
            get(get_host).patch(update_host).delete(delete_host),
        )
        .route("/hosts/{host_id}/pause", post(pause_host))
        .route("/hosts/{host_id}/resume", post(resume_host))
        .route("/hosts/{host_id}/refresh", post(refresh_host))
        .route("/hosts/{host_id}/snapshots", post(create_snapshot))
        .route("/snapshots", get(list_snapshots))
        .route(
            "/snapshots/{snapshot_id}",
            get(get_snapshot)
                .patch(update_snapshot)
                .delete(delete_snapshot),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize))
        .layer(DefaultBodyLimit::max(1024 * 1024));

    let protected_execution = Router::new()
        .route("/hosts/{host_id}/operations", post(execute_operation))
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize))
        .layer(DefaultBodyLimit::max(max_request_bytes));

    let machine_entry = Router::new()
        .route("/registered-hosts/claim", post(claim_registered_host))
        .route(
            "/registered-hosts/{host_id}/connect",
            get(connect_registered_host),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            limit_machine_entry,
        ))
        .layer(DefaultBodyLimit::max(4096));

    Router::new()
        .merge(protected_health)
        .nest("/v1", protected_control)
        .nest("/v1", protected_execution)
        .nest("/v1", machine_entry)
        .layer(middleware::from_fn(audit_request))
        .with_state(state)
}

async fn authorize(State(state): State<AppState>, request: Request<Body>, next: Next) -> Response {
    let valid = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|provided| bool::from(provided.as_bytes().ct_eq(state.api_token.as_bytes())));
    if !valid {
        if !state.security.allow_authentication_failure(&request) {
            return GatewayError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "AUTHENTICATION_RATE_LIMITED",
                "too many failed authentication attempts",
            )
            .retryable(true)
            .into_response();
        }
        return GatewayError::new(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "a valid bearer token is required",
        )
        .into_response();
    }
    next.run(request).await
}

async fn limit_machine_entry(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !state.security.allow_machine_entry(&request) {
        return GatewayError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "MACHINE_ENTRY_RATE_LIMITED",
            "too many registration or connection attempts",
        )
        .retryable(true)
        .into_response();
    }
    next.run(request).await
}

async fn audit_request(request: Request<Body>, next: Next) -> Response {
    let request_id = Uuid::now_v7().to_string();
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let started_at = std::time::Instant::now();
    let mut response = next.run(request).await;
    let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let status = response.status();
    response.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&request_id).expect("UUID request IDs are valid header values"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    tracing::info!(
        %request_id,
        %method,
        %path,
        status = status.as_u16(),
        latency_ms,
        "gateway request completed"
    );
    response
}

async fn health() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn readiness(State(state): State<AppState>) -> Result<StatusCode, GatewayError> {
    state.database.health().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        program: "execution-gateway".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        execution_protocol: PROTOCOL_NAME.to_owned(),
        execution_protocol_version: PROTOCOL_VERSION,
    })
}

async fn create_account(
    State(state): State<AppState>,
    Json(request): Json<CreateE2bAccountRequest>,
) -> Result<Response, GatewayError> {
    let name = validated_name(request.name, "account name")?.expect("account name is required");
    E2bApiKey::new(request.api_key.clone())
        .map_err(|error| GatewayError::bad_request("INVALID_E2B_API_KEY", error.message))?;
    state
        .provider
        .verify_credentials(&request.api_key)
        .await
        .map_err(provider_http_error)?;

    let id = Uuid::now_v7();
    let encrypted = state
        .vault
        .encrypt(&credential_context(id), request.api_key.as_bytes())?;
    let account = state
        .database
        .create_account(
            id,
            &name,
            &credential_fingerprint(&request.api_key),
            &encrypted.ciphertext,
            &encrypted.nonce,
            encrypted.key_version,
            request.is_default,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(account)).into_response())
}

async fn list_accounts(
    State(state): State<AppState>,
) -> Result<Json<Vec<execution_api::E2bAccount>>, GatewayError> {
    Ok(Json(state.database.accounts().await?))
}

async fn get_account(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<execution_api::E2bAccount>, GatewayError> {
    state
        .database
        .account(account_id)
        .await?
        .map(Json)
        .ok_or_else(|| GatewayError::not_found("E2B account"))
}

async fn update_account(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
    Json(request): Json<UpdateE2bAccountRequest>,
) -> Result<Json<execution_api::E2bAccount>, GatewayError> {
    let existing = state
        .database
        .account(account_id)
        .await?
        .ok_or_else(|| GatewayError::not_found("E2B account"))?;
    if existing.is_default && request.enabled == Some(false) && request.is_default != Some(false) {
        return Err(GatewayError::conflict(
            "DEFAULT_ACCOUNT_CANNOT_BE_DISABLED",
            "make another account the default or clear is_default in the same request",
        ));
    }
    if existing.status == E2bAccountStatus::Invalid && request.enabled == Some(true) {
        return Err(GatewayError::conflict(
            "E2B_ACCOUNT_INVALID",
            "verify or replace the E2B credential before enabling this account",
        ));
    }
    if request.is_default == Some(true)
        && existing.status != E2bAccountStatus::Active
        && request.enabled != Some(true)
    {
        return Err(GatewayError::conflict(
            "DEFAULT_ACCOUNT_MUST_BE_ACTIVE",
            "enable the E2B account before making it the default",
        ));
    }
    let name = request
        .name
        .map(|name| validated_name(name, "account name"))
        .transpose()?
        .flatten();
    state
        .database
        .update_account(
            account_id,
            name.as_deref(),
            request.is_default,
            request.enabled,
        )
        .await?
        .map(Json)
        .ok_or_else(|| {
            GatewayError::conflict("ACCOUNT_UPDATE_CONFLICT", "account update is invalid")
        })
}

async fn replace_account_credential(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
    Json(request): Json<ReplaceE2bCredentialRequest>,
) -> Result<Json<execution_api::E2bAccount>, GatewayError> {
    if state.database.account(account_id).await?.is_none() {
        return Err(GatewayError::not_found("E2B account"));
    }
    E2bApiKey::new(request.api_key.clone())
        .map_err(|error| GatewayError::bad_request("INVALID_E2B_API_KEY", error.message))?;
    state
        .provider
        .verify_credentials(&request.api_key)
        .await
        .map_err(provider_http_error)?;
    let encrypted = state
        .vault
        .encrypt(&credential_context(account_id), request.api_key.as_bytes())?;
    state
        .database
        .replace_account_credential(
            account_id,
            &credential_fingerprint(&request.api_key),
            &encrypted.ciphertext,
            &encrypted.nonce,
            encrypted.key_version,
        )
        .await?
        .map(Json)
        .ok_or_else(|| GatewayError::not_found("E2B account"))
}

async fn verify_account(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<execution_api::E2bAccount>, GatewayError> {
    let credential = state
        .database
        .account_credential(account_id, false)
        .await?
        .ok_or_else(|| GatewayError::not_found("E2B account"))?;
    let decrypted = state.vault.decrypt(
        &credential_context(account_id),
        &credential.ciphertext,
        &credential.nonce,
        credential.key_version,
    )?;
    let api_key = str::from_utf8(&decrypted)
        .map_err(|_| GatewayError::internal("stored E2B credential is invalid"))?;
    if let Err(error) = state.provider.verify_credentials(api_key).await {
        if error.kind == E2bErrorKind::Authentication {
            state
                .database
                .mark_account_verification(account_id, false)
                .await?;
        }
        return Err(provider_http_error(error));
    }
    state
        .database
        .mark_account_verification(account_id, true)
        .await?
        .map(Json)
        .ok_or_else(|| GatewayError::not_found("E2B account"))
}

async fn delete_account(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<StatusCode, GatewayError> {
    if state.database.account(account_id).await?.is_none() {
        return Err(GatewayError::not_found("E2B account"));
    }
    if !state.database.delete_account(account_id).await? {
        return Err(GatewayError::conflict(
            "E2B_ACCOUNT_IN_USE",
            "this account is referenced by host or snapshot history; disable it instead",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn create_registered_host(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateRegisteredHostRequest>,
) -> Result<Response, GatewayError> {
    validate_object(&request.metadata, "metadata")?;
    let name = request
        .name
        .clone()
        .map(|name| validated_name(name, "host name"))
        .transpose()?
        .flatten();
    let key = idempotency_key(&headers)?;
    let hash = request_hash(&request)?;
    let token = random_secret("ehr_");
    let expires_at = Utc::now()
        + chrono::Duration::from_std(state.registration_token_ttl)
            .map_err(|_| GatewayError::internal("registration token TTL is invalid"))?;
    let token_hash = Sha256::digest(token.as_bytes());
    let resource = state
        .database
        .create_registered_host(
            Uuid::now_v7(),
            name.as_deref(),
            &request.metadata,
            &token_hash,
            expires_at,
            key,
            &hash,
        )
        .await?;
    if resource.replayed
        && !state
            .database
            .renew_registered_host_token(resource.id, &token_hash, expires_at)
            .await?
    {
        return Err(GatewayError::internal(
            "registered host disappeared during idempotent replay",
        ));
    }
    let host = state
        .database
        .host(resource.id, false)
        .await?
        .ok_or_else(|| GatewayError::internal("created registered host disappeared"))?;
    Ok((
        StatusCode::CREATED,
        Json(RegisteredHostRegistration {
            host,
            registration_token: token,
            registration_token_expires_at: expires_at,
        }),
    )
        .into_response())
}

async fn renew_registered_host_token(
    State(state): State<AppState>,
    Path(host_id): Path<Uuid>,
) -> Result<Json<RenewRegisteredHostTokenResponse>, GatewayError> {
    let token = random_secret("ehr_");
    let expires_at = Utc::now()
        + chrono::Duration::from_std(state.registration_token_ttl)
            .map_err(|_| GatewayError::internal("registration token TTL is invalid"))?;
    if !state
        .database
        .renew_registered_host_token(host_id, &Sha256::digest(token.as_bytes()), expires_at)
        .await?
    {
        return Err(GatewayError::not_found("registered host"));
    }
    Ok(Json(RenewRegisteredHostTokenResponse {
        host_id,
        registration_token: token,
        registration_token_expires_at: expires_at,
    }))
}

async fn claim_registered_host(
    State(state): State<AppState>,
    Json(request): Json<ClaimRegisteredHostRequest>,
) -> Result<Json<ClaimRegisteredHostResponse>, GatewayError> {
    validate_secret(&request.registration_token, "registration token")?;
    validate_daemon_version(&request.daemon_version)?;
    let credential = random_secret("ehc_");
    let host_id = state
        .database
        .claim_registered_host(
            &Sha256::digest(request.registration_token.as_bytes()),
            request.installation_id,
            &request.daemon_version,
            &Sha256::digest(credential.as_bytes()),
        )
        .await?
        .ok_or_else(|| {
            GatewayError::new(
                StatusCode::UNAUTHORIZED,
                "INVALID_REGISTRATION_TOKEN",
                "the registration token is invalid, expired, or already used",
            )
        })?;
    state.connections.disconnect(
        host_id,
        "HOST_CREDENTIAL_ROTATED",
        "a new Host Daemon credential was issued",
    );
    Ok(Json(ClaimRegisteredHostResponse {
        host_id,
        credential,
        websocket_url: registered_websocket_url(&state.public_base_url, host_id)?,
    }))
}

async fn connect_registered_host(
    State(state): State<AppState>,
    Path(host_id): Path<Uuid>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, GatewayError> {
    let credential = bearer_token(&headers).ok_or_else(|| {
        GatewayError::new(
            StatusCode::UNAUTHORIZED,
            "INVALID_HOST_CREDENTIAL",
            "a valid Host Daemon bearer credential is required",
        )
    })?;
    let credential_hash = Sha256::digest(credential.as_bytes()).to_vec();
    if state
        .database
        .authenticate_registered_host(host_id, &credential_hash)
        .await?
        .is_none()
    {
        return Err(GatewayError::new(
            StatusCode::UNAUTHORIZED,
            "INVALID_HOST_CREDENTIAL",
            "the Host Daemon credential is invalid or revoked",
        ));
    }
    Ok(upgrade
        .on_upgrade(move |socket| run_registered_socket(state, host_id, credential_hash, socket))
        .into_response())
}

async fn run_registered_socket(
    state: AppState,
    host_id: Uuid,
    credential_hash: Vec<u8>,
    socket: WebSocket,
) {
    if let Err(error) = run_registered_socket_inner(state, host_id, &credential_hash, socket).await
    {
        tracing::warn!(%host_id, ?error, "registered host connection ended");
    }
}

async fn run_registered_socket_inner(
    state: AppState,
    host_id: Uuid,
    credential_hash: &[u8],
    mut socket: WebSocket,
) -> Result<(), GatewayError> {
    let first = tokio::time::timeout(Duration::from_secs(10), socket.recv())
        .await
        .map_err(|_| {
            GatewayError::bad_request("HANDSHAKE_TIMEOUT", "Host Daemon did not send hello")
        })?
        .ok_or_else(|| {
            GatewayError::bad_request("HANDSHAKE_ENDED", "Host Daemon disconnected before hello")
        })?
        .map_err(|error| GatewayError::bad_request("INVALID_HANDSHAKE", error.to_string()))?;
    let Message::Text(first) = first else {
        return Err(GatewayError::bad_request(
            "INVALID_HANDSHAKE",
            "the first WebSocket message must be a JSON hello message",
        ));
    };
    let hello: HostDaemonMessage = serde_json::from_str(first.as_str())
        .map_err(|error| GatewayError::bad_request("INVALID_HANDSHAKE", error.to_string()))?;
    let HostDaemonMessage::Hello {
        protocol_name,
        protocol_version,
        daemon_version,
        daemon_instance_id,
        descriptor,
    } = hello
    else {
        return Err(GatewayError::bad_request(
            "INVALID_HANDSHAKE",
            "the first Host Daemon message must be hello",
        ));
    };
    if protocol_name != PROTOCOL_NAME || protocol_version != PROTOCOL_VERSION {
        return Err(GatewayError::bad_request(
            "UNSUPPORTED_PROTOCOL",
            format!(
                "Host Daemon protocol {protocol_name}/{protocol_version} is incompatible with {PROTOCOL_NAME}/{PROTOCOL_VERSION}"
            ),
        ));
    }
    validate_daemon_version(&daemon_version)?;
    descriptor
        .validate()
        .map_err(|error| GatewayError::bad_request("INVALID_HOST_DESCRIPTOR", error.to_string()))?;
    if descriptor.host_id.as_str() != host_id.to_string() {
        return Err(GatewayError::bad_request(
            "HOST_ID_MISMATCH",
            "the Host Daemon descriptor belongs to a different execution host",
        ));
    }

    let connection_id = Uuid::now_v7();
    let (outgoing_tx, mut outgoing_rx) = tokio::sync::mpsc::channel(128);
    state
        .connections
        .register(host_id, connection_id, outgoing_tx.clone());
    if !state
        .database
        .mark_registered_connected(
            host_id,
            connection_id,
            daemon_instance_id,
            &daemon_version,
            protocol_version,
            descriptor.as_ref(),
            credential_hash,
        )
        .await?
    {
        state.connections.unregister(host_id, connection_id);
        return Err(GatewayError::new(
            StatusCode::UNAUTHORIZED,
            "HOST_REGISTRATION_REVOKED",
            "the registered host is no longer active",
        ));
    }

    outgoing_tx
        .send(ConnectionOutput::Message(GatewayHostMessage::Welcome {
            heartbeat_interval_ms: u64::try_from(state.heartbeat_interval.as_millis())
                .unwrap_or(u64::MAX),
        }))
        .await
        .map_err(|_| GatewayError::internal("Host Daemon connection closed during handshake"))?;

    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + state.heartbeat_interval,
        state.heartbeat_interval,
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let heartbeat_timeout = state.heartbeat_interval.saturating_mul(3);
    let mut last_activity = tokio::time::Instant::now();
    let session_result = async {
        loop {
            tokio::select! {
                output = outgoing_rx.recv() => {
                    let Some(ConnectionOutput::Message(message)) = output else { break; };
                    send_gateway_message(&mut socket, &message).await?;
                }
                incoming = socket.recv() => {
                    let Some(incoming) = incoming else { break; };
                    last_activity = tokio::time::Instant::now();
                    match incoming {
                        Ok(Message::Text(text)) => match serde_json::from_str::<HostDaemonMessage>(text.as_str()) {
                            Ok(HostDaemonMessage::Response { response }) => {
                                state.connections.complete(host_id, connection_id, *response);
                                state.database.touch_registered_host(host_id, connection_id).await?;
                            }
                            Ok(HostDaemonMessage::Hello { .. }) => {
                                let _ = send_gateway_message(
                                    &mut socket,
                                    &GatewayHostMessage::Disconnect {
                                        code: "DUPLICATE_HELLO".to_owned(),
                                        message: "hello may only be sent once".to_owned(),
                                    },
                                ).await;
                                break;
                            }
                            Err(error) => {
                                let _ = send_gateway_message(
                                    &mut socket,
                                    &GatewayHostMessage::Disconnect {
                                        code: "INVALID_MESSAGE".to_owned(),
                                        message: error.to_string(),
                                    },
                                ).await;
                                break;
                            }
                        },
                        Ok(Message::Pong(_)) => {
                            state.database.touch_registered_host(host_id, connection_id).await?;
                        }
                        Ok(Message::Close(_)) | Err(_) => break,
                        Ok(Message::Binary(_) | Message::Ping(_)) => {}
                    }
                }
                _ = heartbeat.tick() => {
                    if last_activity.elapsed() >= heartbeat_timeout {
                        break;
                    }
                    if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
        Ok::<(), GatewayError>(())
    }
    .await;
    let active = state.connections.unregister(host_id, connection_id);
    if active {
        if let Err(error) = state
            .database
            .mark_registered_disconnected(host_id, connection_id)
            .await
        {
            if session_result.is_ok() {
                return Err(error.into());
            }
            tracing::error!(%host_id, %error, "failed to persist registered host disconnect");
        }
    }
    session_result
}

async fn send_gateway_message(
    socket: &mut WebSocket,
    message: &GatewayHostMessage,
) -> Result<(), GatewayError> {
    let json = serde_json::to_string(message)
        .map_err(|error| GatewayError::internal(error.to_string()))?;
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|error| {
            GatewayError::new(
                StatusCode::BAD_GATEWAY,
                "HOST_DISCONNECTED",
                error.to_string(),
            )
            .retryable(true)
        })
}

async fn create_host(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateExecutionHostRequest>,
) -> Result<Response, GatewayError> {
    validate_object(&request.metadata, "metadata")?;
    let name = request
        .name
        .clone()
        .map(|name| validated_name(name, "host name"))
        .transpose()?
        .flatten();
    let (account_id, source_type, source_snapshot_id, ram_mb) = match request.source {
        E2bHostSource::Base {
            e2b_account_id,
            ram,
        } => {
            let account_id = match e2b_account_id {
                Some(id) => id,
                None => state.database.default_account_id().await?.ok_or_else(|| {
                    GatewayError::conflict(
                        "NO_DEFAULT_E2B_ACCOUNT",
                        "provide an E2B account or configure a default account",
                    )
                })?,
            };
            require_active_account(&state, account_id).await?;
            let ram_mb = ram.unwrap_or(DEFAULT_EXECUTION_BASE_RAM_MB);
            validate_base_ram(ram_mb)?;
            (account_id, "base", None, Some(ram_mb))
        }
        E2bHostSource::Snapshot { snapshot_id } => {
            let snapshot = state
                .database
                .snapshot_source(snapshot_id)
                .await?
                .ok_or_else(|| {
                    GatewayError::conflict(
                        "SNAPSHOT_NOT_READY",
                        "the gateway snapshot does not exist or is not ready",
                    )
                })?;
            require_active_account(&state, snapshot.e2b_account_id).await?;
            (
                snapshot.e2b_account_id,
                "snapshot",
                Some(snapshot.snapshot_id),
                None,
            )
        }
    };
    let timeout = request
        .timeout_seconds
        .unwrap_or(state.default_host_timeout_seconds);
    validate_timeout(timeout)?;
    let network_access = request.network_access.unwrap_or(true);
    let key = idempotency_key(&headers)?;
    let hash = request_hash(&request)?;
    let resource = state
        .database
        .create_host(
            Uuid::now_v7(),
            name.as_deref(),
            &request.metadata,
            account_id,
            source_type,
            source_snapshot_id,
            timeout,
            network_access,
            ram_mb,
            key,
            &hash,
        )
        .await?;
    kick(&state);
    let host = state
        .database
        .host(resource.id, true)
        .await?
        .ok_or_else(|| GatewayError::internal("created host disappeared"))?;
    Ok((StatusCode::ACCEPTED, Json(host)).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct HostQuery {
    state: Option<String>,
    kind: Option<String>,
    e2b_account_id: Option<Uuid>,
    #[serde(default)]
    include_deleted: bool,
}

async fn list_hosts(
    State(state): State<AppState>,
    Query(query): Query<HostQuery>,
) -> Result<Json<Vec<execution_api::ExecutionHost>>, GatewayError> {
    if let Some(value) = &query.state {
        validate_host_state(value)?;
    }
    if let Some(value) = &query.kind {
        validate_host_kind(value)?;
    }
    Ok(Json(
        state
            .database
            .hosts(&HostListFilter {
                state: query.state,
                kind: query.kind,
                e2b_account_id: query.e2b_account_id,
                include_deleted: query.include_deleted,
            })
            .await?,
    ))
}

async fn get_host(
    State(state): State<AppState>,
    Path(host_id): Path<Uuid>,
) -> Result<Json<execution_api::ExecutionHost>, GatewayError> {
    state
        .database
        .host(host_id, true)
        .await?
        .map(Json)
        .ok_or_else(|| GatewayError::not_found("execution host"))
}

async fn update_host(
    State(state): State<AppState>,
    Path(host_id): Path<Uuid>,
    Json(request): Json<UpdateExecutionHostRequest>,
) -> Result<Json<execution_api::ExecutionHost>, GatewayError> {
    let name = request
        .name
        .map(|name| validated_name(name, "host name"))
        .transpose()?
        .flatten();
    if let Some(metadata) = &request.metadata {
        validate_object(metadata, "metadata")?;
    }
    state
        .database
        .update_host(host_id, name.as_deref(), request.metadata.as_ref())
        .await?
        .map(Json)
        .ok_or_else(|| GatewayError::not_found("execution host"))
}

async fn pause_host(
    State(state): State<AppState>,
    Path(host_id): Path<Uuid>,
) -> Result<Response, GatewayError> {
    require_e2b_host(&state, host_id, "pause").await?;
    let host = state
        .database
        .request_host_state(host_id, "paused", "pausing")
        .await?
        .ok_or_else(|| GatewayError::not_found("execution host"))?;
    kick(&state);
    Ok((StatusCode::ACCEPTED, Json(host)).into_response())
}

async fn resume_host(
    State(state): State<AppState>,
    Path(host_id): Path<Uuid>,
) -> Result<Response, GatewayError> {
    require_e2b_host(&state, host_id, "resume").await?;
    let host = state
        .database
        .request_host_state(host_id, "ready", "resuming")
        .await?
        .ok_or_else(|| GatewayError::not_found("execution host"))?;
    kick(&state);
    Ok((StatusCode::ACCEPTED, Json(host)).into_response())
}

async fn refresh_host(
    State(state): State<AppState>,
    Path(host_id): Path<Uuid>,
) -> Result<Response, GatewayError> {
    let current = state
        .database
        .host(host_id, false)
        .await?
        .ok_or_else(|| GatewayError::not_found("execution host"))?;
    if current.kind == ExecutionHostKind::Registered {
        if !state.connections.is_connected(host_id) {
            state
                .database
                .mark_registered_unavailable(
                    host_id,
                    "HOST_DAEMON_DISCONNECTED",
                    "the Host Daemon is not connected",
                )
                .await?;
        }
        let refreshed = state
            .database
            .host(host_id, false)
            .await?
            .ok_or_else(|| GatewayError::not_found("execution host"))?;
        return Ok((StatusCode::OK, Json(refreshed)).into_response());
    }
    let host = state
        .database
        .schedule_host_reconcile(host_id)
        .await?
        .ok_or_else(|| GatewayError::not_found("execution host"))?;
    kick(&state);
    Ok((StatusCode::ACCEPTED, Json(host)).into_response())
}

async fn delete_host(
    State(state): State<AppState>,
    Path(host_id): Path<Uuid>,
) -> Result<Response, GatewayError> {
    let existing = state
        .database
        .host(host_id, true)
        .await?
        .ok_or_else(|| GatewayError::not_found("execution host"))?;
    if existing.state == ExecutionHostState::Deleted {
        return Ok((StatusCode::ACCEPTED, Json(existing)).into_response());
    }
    if existing.kind == ExecutionHostKind::Registered {
        state.connections.disconnect(
            host_id,
            "HOST_REGISTRATION_DELETED",
            "this Registered Host was deleted from the Execution Gateway",
        );
        if !state.database.delete_registered_host(host_id).await? {
            return Err(GatewayError::not_found("registered host"));
        }
        let deleted = state
            .database
            .host(host_id, true)
            .await?
            .ok_or_else(|| GatewayError::internal("deleted registered host disappeared"))?;
        return Ok((StatusCode::ACCEPTED, Json(deleted)).into_response());
    }
    let host = state
        .database
        .request_host_state(host_id, "deleted", "deleting")
        .await?
        .ok_or_else(|| GatewayError::not_found("execution host"))?;
    kick(&state);
    Ok((StatusCode::ACCEPTED, Json(host)).into_response())
}

async fn execute_operation(
    State(state): State<AppState>,
    Path(host_id): Path<Uuid>,
    Json(request): Json<RequestEnvelope>,
) -> Result<Json<ResponseEnvelope>, GatewayError> {
    let _permit = state.security.try_operation().ok_or_else(|| {
        GatewayError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "GATEWAY_BUSY",
            "the gateway has too many execution operations in flight",
        )
        .retryable(true)
    })?;
    let mut host = state
        .database
        .host(host_id, false)
        .await?
        .ok_or_else(|| GatewayError::not_found("execution host"))?;
    if host.kind == ExecutionHostKind::E2b
        && matches!(
            host.state,
            ExecutionHostState::Paused | ExecutionHostState::Resuming
        )
        && host.desired_state != execution_api::DesiredHostState::Deleted
    {
        state.database.resume_host_on_use(host_id).await?;
        kick(&state);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            host = state
                .database
                .host(host_id, false)
                .await?
                .ok_or_else(|| GatewayError::not_found("execution host"))?;
            if host.state != ExecutionHostState::Resuming {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(GatewayError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "HOST_RESUMING",
                    "host is still resuming; retry the same operation",
                )
                .retryable(true));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    if host.state != ExecutionHostState::Ready
        || host.desired_state != execution_api::DesiredHostState::Ready
    {
        return Err(GatewayError::conflict(
            "HOST_NOT_READY",
            format!("host is currently {:?}", host.state).to_lowercase(),
        ));
    }
    if host.kind == ExecutionHostKind::Registered {
        return state
            .connections
            .execute(host_id, request)
            .await
            .map(Json)
            .map_err(host_connection_http_error);
    }
    let work = state
        .database
        .e2b_host_work(host_id)
        .await?
        .ok_or_else(|| {
            GatewayError::conflict("E2B_ACCOUNT_UNAVAILABLE", "host account is unavailable")
        })?;
    if work.account_status != E2bAccountStatus::Active {
        return Err(GatewayError::conflict(
            "E2B_ACCOUNT_UNAVAILABLE",
            "host account is disabled or invalid",
        ));
    }
    let sandbox_id = work
        .e2b_sandbox_id
        .as_deref()
        .ok_or_else(|| GatewayError::conflict("HOST_NOT_READY", "host has no E2B resource"))?;
    let decrypted = state.vault.decrypt(
        &credential_context(work.e2b_account_id),
        &work.credential.ciphertext,
        &work.credential.nonce,
        work.credential.key_version,
    )?;
    let api_key = str::from_utf8(&decrypted)
        .map_err(|_| GatewayError::internal("stored E2B credential is invalid"))?;
    let result = state
        .provider
        .execute(
            api_key,
            sandbox_id,
            ExecutionHostId::new(host_id.to_string())
                .map_err(|error| GatewayError::internal(error.to_string()))?,
            work.timeout_seconds,
            request,
        )
        .await;
    match result {
        Ok(result) => {
            state
                .database
                .complete_host_ready(host_id, &result.descriptor)
                .await?;
            Ok(Json(result.response))
        }
        Err(error) => {
            let state_name = if error.kind == E2bErrorKind::NotFound {
                "lost"
            } else {
                "unavailable"
            };
            state
                .database
                .fail_host(
                    host_id,
                    Some(&execution_api::DesiredHostState::Ready),
                    HostFailure {
                        observed_state: state_name,
                        code: "EXECUTION_TRANSPORT_FAILED",
                        message: &error.message,
                        retryable: error.retryable,
                        ambiguous: false,
                    },
                )
                .await?;
            Err(provider_http_error(error))
        }
    }
}

async fn create_snapshot(
    State(state): State<AppState>,
    Path(host_id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<CreateSnapshotRequest>,
) -> Result<Response, GatewayError> {
    validate_object(&request.metadata, "metadata")?;
    let name = request
        .name
        .clone()
        .map(|name| validated_name(name, "snapshot name"))
        .transpose()?
        .flatten();
    let key = idempotency_key(&headers)?;
    let hash = request_hash(&(host_id, &request))?;
    let resource = state
        .database
        .create_snapshot(
            Uuid::now_v7(),
            host_id,
            name.as_deref(),
            &request.metadata,
            key,
            &hash,
        )
        .await?;
    kick(&state);
    let snapshot = state
        .database
        .snapshot(resource.id, true)
        .await?
        .ok_or_else(|| GatewayError::internal("created snapshot disappeared"))?;
    Ok((StatusCode::ACCEPTED, Json(snapshot)).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct SnapshotQuery {
    state: Option<String>,
    e2b_account_id: Option<Uuid>,
    source_host_id: Option<Uuid>,
    #[serde(default)]
    include_deleted: bool,
}

async fn list_snapshots(
    State(state): State<AppState>,
    Query(query): Query<SnapshotQuery>,
) -> Result<Json<Vec<execution_api::E2bSnapshot>>, GatewayError> {
    if let Some(value) = &query.state {
        validate_snapshot_state(value)?;
    }
    Ok(Json(
        state
            .database
            .snapshots(&SnapshotListFilter {
                state: query.state,
                e2b_account_id: query.e2b_account_id,
                source_host_id: query.source_host_id,
                include_deleted: query.include_deleted,
            })
            .await?,
    ))
}

async fn get_snapshot(
    State(state): State<AppState>,
    Path(snapshot_id): Path<Uuid>,
) -> Result<Json<execution_api::E2bSnapshot>, GatewayError> {
    state
        .database
        .snapshot(snapshot_id, true)
        .await?
        .map(Json)
        .ok_or_else(|| GatewayError::not_found("snapshot"))
}

async fn update_snapshot(
    State(state): State<AppState>,
    Path(snapshot_id): Path<Uuid>,
    Json(request): Json<UpdateSnapshotRequest>,
) -> Result<Json<execution_api::E2bSnapshot>, GatewayError> {
    let name = request
        .name
        .map(|name| validated_name(name, "snapshot name"))
        .transpose()?
        .flatten();
    if let Some(metadata) = &request.metadata {
        validate_object(metadata, "metadata")?;
    }
    state
        .database
        .update_snapshot(snapshot_id, name.as_deref(), request.metadata.as_ref())
        .await?
        .map(Json)
        .ok_or_else(|| GatewayError::not_found("snapshot"))
}

async fn delete_snapshot(
    State(state): State<AppState>,
    Path(snapshot_id): Path<Uuid>,
) -> Result<Response, GatewayError> {
    if let Some(snapshot) = state.database.snapshot(snapshot_id, true).await? {
        if snapshot.state == execution_api::SnapshotState::Deleted {
            return Ok((StatusCode::ACCEPTED, Json(snapshot)).into_response());
        }
    }
    let snapshot = state
        .database
        .request_snapshot_delete(snapshot_id)
        .await?
        .ok_or_else(|| GatewayError::not_found("snapshot"))?;
    kick(&state);
    Ok((StatusCode::ACCEPTED, Json(snapshot)).into_response())
}

async fn require_active_account(state: &AppState, id: Uuid) -> Result<(), GatewayError> {
    let account = state
        .database
        .account(id)
        .await?
        .ok_or_else(|| GatewayError::not_found("E2B account"))?;
    if account.status != E2bAccountStatus::Active {
        return Err(GatewayError::conflict(
            "E2B_ACCOUNT_UNAVAILABLE",
            "the selected E2B account is not active",
        ));
    }
    Ok(())
}

async fn require_e2b_host(
    state: &AppState,
    host_id: Uuid,
    action: &str,
) -> Result<(), GatewayError> {
    let host = state
        .database
        .host(host_id, false)
        .await?
        .ok_or_else(|| GatewayError::not_found("execution host"))?;
    if host.kind != ExecutionHostKind::E2b {
        return Err(GatewayError::conflict(
            "HOST_OPERATION_UNSUPPORTED",
            format!("Registered Hosts do not support {action}"),
        ));
    }
    Ok(())
}

fn host_connection_http_error(error: HostConnectionError) -> GatewayError {
    match error {
        HostConnectionError::Offline | HostConnectionError::Disconnected => GatewayError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "HOST_DISCONNECTED",
            error.to_string(),
        )
        .retryable(true),
        HostConnectionError::Busy => GatewayError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "HOST_BUSY",
            error.to_string(),
        )
        .retryable(true),
        HostConnectionError::Timeout => GatewayError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "HOST_OPERATION_TIMEOUT",
            error.to_string(),
        )
        .retryable(true),
        HostConnectionError::DuplicateRequest => {
            GatewayError::conflict("REQUEST_ALREADY_IN_FLIGHT", error.to_string())
        }
    }
}

fn kick(state: &AppState) {
    let reconciler = state.reconciler.clone();
    tokio::spawn(async move {
        if let Err(error) = reconciler.reconcile_once().await {
            tracing::error!(%error, "immediate lifecycle reconciliation failed");
        }
    });
}

fn validated_name(value: String, field: &str) -> Result<Option<String>, GatewayError> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(GatewayError::bad_request(
            "INVALID_NAME",
            format!("{field} must not be blank or contain surrounding whitespace"),
        ));
    }
    if value.chars().count() > 200 {
        return Err(GatewayError::bad_request(
            "INVALID_NAME",
            format!("{field} must not exceed 200 characters"),
        ));
    }
    Ok(Some(value))
}

fn validate_object(value: &Value, field: &str) -> Result<(), GatewayError> {
    if !value.is_object() {
        return Err(GatewayError::bad_request(
            "INVALID_METADATA",
            format!("{field} must be a JSON object"),
        ));
    }
    Ok(())
}

fn validate_timeout(timeout: u64) -> Result<(), GatewayError> {
    if !(1..=86_400).contains(&timeout) {
        return Err(GatewayError::bad_request(
            "INVALID_TIMEOUT",
            "timeout_seconds must be between 1 and 86400",
        ));
    }
    Ok(())
}

fn validate_base_ram(ram_mb: u32) -> Result<(), GatewayError> {
    if !EXECUTION_BASE_RAM_OPTIONS_MB.contains(&ram_mb) {
        return Err(GatewayError::bad_request(
            "INVALID_RAM",
            "source.ram must be one of 1024, 2048, 4096, or 8192",
        ));
    }
    Ok(())
}

fn idempotency_key(headers: &HeaderMap) -> Result<&str, GatewayError> {
    let value = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            GatewayError::bad_request(
                "IDEMPOTENCY_KEY_REQUIRED",
                "an Idempotency-Key header is required",
            )
        })?;
    if value.trim().is_empty() || value.trim() != value || value.len() > 255 {
        return Err(GatewayError::bad_request(
            "INVALID_IDEMPOTENCY_KEY",
            "Idempotency-Key must be 1 to 255 characters without surrounding whitespace",
        ));
    }
    Ok(value)
}

fn request_hash(value: &impl serde::Serialize) -> Result<String, GatewayError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| GatewayError::internal(format!("cannot hash request: {error}")))?;
    Ok(hex_digest(&Sha256::digest(bytes)))
}

fn credential_fingerprint(value: &str) -> String {
    hex_digest(&Sha256::digest(value.as_bytes()))
}

fn random_secret(prefix: &str) -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
}

fn validate_secret(value: &str, name: &str) -> Result<(), GatewayError> {
    if value.trim() != value || value.len() < 32 || value.len() > 255 {
        return Err(GatewayError::bad_request(
            "INVALID_SECRET",
            format!("{name} has an invalid format"),
        ));
    }
    Ok(())
}

fn validate_daemon_version(value: &str) -> Result<(), GatewayError> {
    if value.trim().is_empty() || value.trim() != value || value.len() > 100 {
        return Err(GatewayError::bad_request(
            "INVALID_DAEMON_VERSION",
            "daemon_version must be 1 to 100 characters without surrounding whitespace",
        ));
    }
    Ok(())
}

fn registered_websocket_url(base: &url::Url, host_id: Uuid) -> Result<String, GatewayError> {
    let mut url = base.clone();
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        "ws" => "ws",
        "wss" => "wss",
        _ => {
            return Err(GatewayError::internal(
                "gateway public URL must use http, https, ws, or wss",
            ));
        }
    };
    url.set_scheme(scheme)
        .map_err(|_| GatewayError::internal("cannot construct Host Daemon WebSocket URL"))?;
    url.set_path(&format!("/v1/registered-hosts/{host_id}/connect"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write;

    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        },
    )
}

fn validate_host_state(value: &str) -> Result<(), GatewayError> {
    if matches!(
        value,
        "provisioning"
            | "ready"
            | "pausing"
            | "paused"
            | "resuming"
            | "unavailable"
            | "deleting"
            | "deleted"
            | "failed"
            | "lost"
    ) {
        Ok(())
    } else {
        Err(GatewayError::bad_request(
            "INVALID_HOST_STATE",
            "unknown host state filter",
        ))
    }
}

fn validate_host_kind(value: &str) -> Result<(), GatewayError> {
    if matches!(value, "e2b" | "registered") {
        Ok(())
    } else {
        Err(GatewayError::bad_request(
            "INVALID_HOST_KIND",
            "unknown host kind filter",
        ))
    }
}

fn validate_snapshot_state(value: &str) -> Result<(), GatewayError> {
    if matches!(
        value,
        "creating" | "ready" | "deleting" | "deleted" | "failed"
    ) {
        Ok(())
    } else {
        Err(GatewayError::bad_request(
            "INVALID_SNAPSHOT_STATE",
            "unknown snapshot state filter",
        ))
    }
}

#[cfg(test)]
mod tests {
    use execution_api::E2bHostSource;

    use super::*;

    #[test]
    fn hashes_are_stable_and_keys_are_not_exposed() {
        assert_eq!(credential_fingerprint("key"), credential_fingerprint("key"));
        assert!(!credential_fingerprint("key").contains("key"));
    }

    #[test]
    fn arbitrary_template_ids_are_rejected_by_the_http_contract() {
        let result = serde_json::from_value::<CreateExecutionHostRequest>(serde_json::json!({
            "source": {"type": "base", "template_id": "not-allowed"}
        }));
        assert!(result.is_err());
    }

    #[test]
    fn base_and_gateway_snapshot_sources_are_accepted() {
        let base = serde_json::from_value::<E2bHostSource>(serde_json::json!({"type": "base"}));
        assert!(base.is_ok());
        let snapshot = serde_json::from_value::<E2bHostSource>(serde_json::json!({
            "type": "snapshot",
            "snapshot_id": Uuid::now_v7()
        }));
        assert!(snapshot.is_ok());
    }
}
