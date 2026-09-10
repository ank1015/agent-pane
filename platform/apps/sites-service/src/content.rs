//! Static content is served by a separate listener, with no internal API routes.
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::Body,
    extract::{Path, Request, State, rejection::PathRejection},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::Semaphore;
use url::Url;
use uuid::Uuid;

use crate::{
    Error, Result, SiteService,
    bundles::{Kind, digest},
};

#[derive(Clone)]
pub struct ContentConfig {
    pub bind_address: SocketAddr,
    pub public_origin: String,
    pub dashboard_origin: String,
}

impl ContentConfig {
    pub fn new(
        bind_address: SocketAddr,
        public_origin: &str,
        dashboard_origin: &str,
    ) -> Result<Self> {
        let public_origin = origin(public_origin)?;
        let dashboard_origin = origin(dashboard_origin)?;
        if public_origin == dashboard_origin {
            return Err(Error::Config(
                "Content and dashboard origins must be different",
            ));
        }
        Ok(Self {
            bind_address,
            public_origin,
            dashboard_origin,
        })
    }
}

fn origin(value: &str) -> Result<String> {
    let url = Url::parse(value).map_err(|_| Error::Config("Invalid content/dashboard origin"))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if (!matches!(url.scheme(), "https") && !(url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
    {
        return Err(Error::Config(
            "Origins require HTTPS (HTTP allowed on loopback), with no credentials, paths, queries, or fragments",
        ));
    }
    Ok(url.origin().ascii_serialization())
}

#[derive(Clone)]
pub struct ContentHost {
    pub config: ContentConfig,
    key: Arc<zeroize::Zeroizing<Vec<u8>>>,
}

#[derive(Serialize, Deserialize)]
struct Claims {
    version: u32,
    site_id: Uuid,
    release_id: Uuid,
    expires_at: u64,
    nonce: Uuid,
}

#[derive(Serialize, Deserialize)]
pub struct ContentAccess {
    pub url: String,
    pub expires_at: u64,
    pub dashboard_origin: String,
}

impl ContentHost {
    pub fn new(config: ContentConfig, api_token: &str) -> Result<Self> {
        crate::config::validate_token(api_token)?;
        // Domain separation: never use the API token itself as a browser credential.
        let mut mac =
            Hmac::<Sha256>::new_from_slice(api_token.as_bytes()).map_err(|_| Error::Storage)?;
        mac.update(b"sites-content-access-v1");
        let key = Arc::new(zeroize::Zeroizing::new(
            mac.finalize().into_bytes().to_vec(),
        ));
        Ok(Self { config, key })
    }

    pub async fn issue(
        &self,
        service: &SiteService,
        site: Uuid,
        release: Uuid,
        ttl: u64,
    ) -> Result<ContentAccess> {
        if !(30..=3600).contains(&ttl) {
            return Err(Error::Invalid(
                "Content access ttl_seconds must be 30–3600.",
            ));
        }
        service.require_ready(site).await?;
        let bundle = service.bundle(site, Kind::Release, release).await?;
        if bundle.status != "ready" {
            return Err(Error::Conflict("BUNDLE_NOT_READY", "Release is not ready."));
        }
        let manifest = bundle.descriptor.manifest.ok_or(Error::Storage)?;
        let entry = manifest
            .frontend_entrypoint
            .strip_prefix("public/")
            .ok_or(Error::Storage)?;
        let expires_at = now()? + ttl;
        let claims = Claims {
            version: 1,
            site_id: site,
            release_id: release,
            expires_at,
            nonce: Uuid::new_v4(),
        };
        let payload =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).map_err(|_| Error::Storage)?);
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| Error::Storage)?;
        mac.update(payload.as_bytes());
        let ticket = format!(
            "{payload}.{}",
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        );
        Ok(ContentAccess {
            dashboard_origin: self.config.dashboard_origin.clone(),
            url: format!(
                "{}/content/{ticket}/{site}/{release}/{entry}",
                self.config.public_origin
            ),
            expires_at,
        })
    }

    fn verify(&self, ticket: &str, site: Uuid, release: Uuid) -> Result<()> {
        if ticket.len() > 1024 {
            return Err(Error::Unauthorized);
        }
        let (payload, signature) = ticket.split_once('.').ok_or(Error::Unauthorized)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| Error::Unauthorized)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| Error::Storage)?;
        mac.update(payload.as_bytes());
        mac.verify_slice(&signature)
            .map_err(|_| Error::Unauthorized)?;
        let claims: Claims = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(payload)
                .map_err(|_| Error::Unauthorized)?,
        )
        .map_err(|_| Error::Unauthorized)?;
        if claims.version != 1
            || claims.site_id != site
            || claims.release_id != release
            || claims.expires_at <= now()?
        {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }
}

fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Storage)?
        .as_secs())
}

#[derive(Clone)]
struct App {
    service: SiteService,
    host: ContentHost,
    capacity: Arc<Semaphore>,
}

pub fn router(service: SiteService, host: ContentHost) -> Router {
    let state = App {
        service,
        host,
        capacity: Arc::new(Semaphore::new(32)),
    };
    Router::new()
        .route("/content/{ticket}/{site}/{release}/{*path}", get(asset))
        .fallback(|| async { Error::ArtifactNotFound.into_response() })
        .layer(middleware::from_fn_with_state(
            state.clone(),
            content_policy,
        ))
        .layer(middleware::from_fn(crate::http::audit))
        .with_state(state)
}

async fn content_policy(State(app): State<App>, request: Request, next: Next) -> Response {
    // Ignore forwarded-host headers; the proxy must preserve the configured Host.
    let expected = app
        .host
        .config
        .public_origin
        .split_once("://")
        .map(|(_, host)| host)
        .unwrap_or("");
    if request.headers().get("host").and_then(|h| h.to_str().ok()) != Some(expected) {
        return Error::Invalid("Content Host does not match the configured origin.")
            .into_response();
    }
    if request.uri().query().is_some() {
        return Error::Invalid("Content URLs do not accept query parameters.").into_response();
    }
    let Ok(_permit) = app.capacity.try_acquire() else {
        return Error::Storage.into_response();
    };
    let mut response = next.run(request).await;
    let origin = &app.host.config.public_origin;
    let csp = format!(
        "default-src 'none'; script-src 'unsafe-inline' {origin}; style-src 'unsafe-inline' {origin}; img-src {origin} data: blob:; font-src {origin} data:; media-src {origin} blob:; connect-src 'none'; worker-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors {}; sandbox allow-scripts",
        app.host.config.dashboard_origin
    );
    let headers = response.headers_mut();
    headers.insert(
        "content-security-policy",
        HeaderValue::from_str(&csp).expect("validated origins"),
    );
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=(), usb=()"),
    );
    // Module scripts/fonts fetch from an opaque sandbox origin. No cookies or
    // credentialed CORS are allowed; every asset still needs the scoped ticket.
    headers.insert(
        "access-control-allow-origin",
        HeaderValue::from_static("null"),
    );
    headers.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("cross-origin"),
    );
    response
}

async fn asset(
    State(app): State<App>,
    path: std::result::Result<Path<(String, Uuid, Uuid, String)>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response> {
    let Path((ticket, site, release, path)) =
        path.map_err(|_| Error::Invalid("Invalid content path."))?;
    app.host.verify(&ticket, site, release)?;
    app.service.require_ready(site).await?;
    crate::bundles::validate_path(&path)?;
    let mut bytes = app
        .service
        .read_bundle_file(site, Kind::Release, release, format!("public/{path}"))
        .await?;
    if matches!(path.rsplit('.').next(), Some("html" | "htm")) {
        let html = String::from_utf8(bytes).map_err(|_| Error::Invalid("HTML must be UTF-8."))?;
        let sdk = include_str!("frontend_sdk.js").replace(
            "__DASHBOARD_ORIGIN__",
            &serde_json::to_string(&app.host.config.dashboard_origin)
                .map_err(|_| Error::Storage)?,
        );
        // Establish standards mode before running the SDK. The original doctype
        // is harmless when repeated; application scripts see callBackend early.
        bytes = format!("<!doctype html><script>{sdk}</script>{html}").into_bytes();
    }
    let etag = format!("\"{}\"", digest(&bytes));
    let unchanged = headers
        .get("if-none-match")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h == etag);
    let mut response = if unchanged {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        Body::from(bytes).into_response()
    };
    response
        .headers_mut()
        .insert("etag", HeaderValue::from_str(&etag).expect("digest"));
    // Revalidate each request so suspension and ticket expiry take effect even
    // for immutable assets. Nothing is placed in a shared CDN cache.
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static("private, no-cache"),
    );
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static(mime(&path)));
    Ok(response)
}

fn mime(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_origins() {
        for invalid in [
            "https://a.test/path",
            "https://u:p@a.test",
            "https://a.test?x=1",
            "http://a.test",
            "data:text/html,hi",
        ] {
            assert!(origin(invalid).is_err());
        }
        assert!(
            ContentConfig::new(
                "127.0.0.1:3103".parse().unwrap(),
                "https://a.test",
                "https://a.test/"
            )
            .is_err()
        );
    }
    #[test]
    fn expired_tickets_are_rejected() {
        let config = ContentConfig::new(
            "127.0.0.1:3103".parse().unwrap(),
            "https://content.test",
            "https://dashboard.test",
        )
        .unwrap();
        let host = ContentHost::new(config, &"x".repeat(32)).unwrap();
        let site = Uuid::new_v4();
        let release = Uuid::new_v4();
        let claims = Claims {
            version: 1,
            site_id: site,
            release_id: release,
            expires_at: 1,
            nonce: Uuid::new_v4(),
        };
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let mut mac = Hmac::<Sha256>::new_from_slice(&host.key).unwrap();
        mac.update(payload.as_bytes());
        let ticket = format!(
            "{payload}.{}",
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        );
        assert!(host.verify(&ticket, site, release).is_err());
    }
}
