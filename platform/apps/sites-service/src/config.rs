use std::{env, net::SocketAddr, path::PathBuf};

use crate::{Error, Result};

pub struct Config {
    pub bind_address: SocketAddr,
    pub data_dir: PathBuf,
    pub api_token: zeroize::Zeroizing<String>,
    pub content: Option<crate::content::ContentConfig>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let bind_address = env::var("SITES_BIND_ADDRESS")
            .unwrap_or_else(|_| "127.0.0.1:3102".into())
            .parse()
            .map_err(|_| Error::Config("SITES_BIND_ADDRESS must be a socket address"))?;
        let data_dir = PathBuf::from(required("SITES_DATA_DIR")?);
        if !data_dir.is_absolute() {
            return Err(Error::Config("SITES_DATA_DIR must be an absolute path"));
        }
        let api_token = zeroize::Zeroizing::new(required("SITES_API_TOKEN")?);
        validate_token(&api_token)?;
        let content = match (
            env::var("SITES_CONTENT_ORIGIN"),
            env::var("SITES_DASHBOARD_ORIGIN"),
        ) {
            (Err(env::VarError::NotPresent), Err(env::VarError::NotPresent)) => None,
            (Ok(origin), Ok(dashboard)) if origin.is_empty() && dashboard.is_empty() => None,
            (Ok(origin), Ok(dashboard)) => {
                let bind = env::var("SITES_CONTENT_BIND_ADDRESS")
                    .unwrap_or_else(|_| "127.0.0.1:3103".into())
                    .parse()
                    .map_err(|_| Error::Config("Invalid SITES_CONTENT_BIND_ADDRESS"))?;
                if bind == bind_address {
                    return Err(Error::Config(
                        "API and content listeners must use different addresses",
                    ));
                }
                Some(crate::content::ContentConfig::new(
                    bind, &origin, &dashboard,
                )?)
            }
            _ => {
                return Err(Error::Config(
                    "Set both SITES_CONTENT_ORIGIN and SITES_DASHBOARD_ORIGIN",
                ));
            }
        };
        Ok(Self {
            bind_address,
            data_dir,
            api_token,
            content,
        })
    }
}

pub(crate) fn validate_token(token: &str) -> Result<()> {
    if !(32..=256).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Error::Config(
            "SITES_API_TOKEN must contain 32–256 visible ASCII characters",
        ));
    }
    Ok(())
}

fn required(name: &'static str) -> Result<String> {
    let value = env::var(name).map_err(|_| Error::Config(name))?;
    if value.is_empty() || value.trim() != value {
        return Err(Error::Config(name));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::validate_token;

    #[test]
    fn rejects_weak_or_malformed_tokens() {
        for token in [
            "",
            "short",
            &"a".repeat(257),
            &format!("{}\n", "a".repeat(32)),
        ] {
            assert!(validate_token(token).is_err());
        }
        assert!(validate_token(&"a".repeat(32)).is_ok());
    }
}
