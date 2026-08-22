use std::io::{self, Read};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use serde_json::Value;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    account::{AccountService, ProviderCredentials, ProviderKind},
    config::AppConfig,
    db::{Database, ProviderAccount},
    gateway::validate_provider_config,
};

#[derive(Parser)]
#[command(
    name = "llm-gateway",
    version,
    about = "Personal, multi-provider LLM gateway"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the HTTP gateway (the default command).
    Serve,
    /// Manage encrypted provider accounts locally.
    Account(AccountArgs),
}

#[derive(Args)]
pub struct AccountArgs {
    #[command(subcommand)]
    pub command: AccountCommand,
}

#[derive(Subcommand)]
pub enum AccountCommand {
    /// Add an account and encrypted credentials.
    Add {
        #[arg(long, value_enum)]
        provider: ProviderKind,
        #[arg(long)]
        name: String,
        /// Make this account the provider default. The first enabled account is automatic.
        #[arg(long)]
        default: bool,
        #[arg(long)]
        disabled: bool,
        /// Non-secret provider configuration as a JSON object.
        #[arg(long, default_value = "{}")]
        config: String,
        /// Read a tagged credential JSON object from stdin instead of prompting.
        #[arg(long)]
        credentials_stdin: bool,
    },
    /// List account metadata without credentials.
    List {
        #[arg(long, value_enum)]
        provider: Option<ProviderKind>,
    },
    /// Select an enabled account as its provider's default.
    SetDefault { account_id: Uuid },
    /// Enable an account.
    Enable { account_id: Uuid },
    /// Disable a non-default account.
    Disable { account_id: Uuid },
    /// Replace the non-secret provider configuration JSON.
    SetConfig {
        account_id: Uuid,
        #[arg(long)]
        config: String,
    },
    /// Replace encrypted credentials and increment their cache version.
    RotateCredentials {
        account_id: Uuid,
        #[arg(long)]
        credentials_stdin: bool,
    },
    /// Permanently remove an account and its encrypted credential record.
    Remove { account_id: Uuid },
}

pub async fn run_account(command: AccountCommand, config: &AppConfig) -> Result<()> {
    let database = Database::connect(&config.database)
        .await
        .context("connect to PostgreSQL")?;
    database
        .migrate()
        .await
        .context("run database migrations")?;
    let accounts = AccountService::new(database.clone(), config.vault.clone());

    match command {
        AccountCommand::Add {
            provider,
            name,
            default,
            disabled,
            config,
            credentials_stdin,
        } => {
            let config = parse_config(&config)?;
            validate_provider_config(provider, &config).map_err(anyhow::Error::msg)?;
            let credentials = read_credentials(provider, credentials_stdin)?;
            let account = accounts
                .create(provider, name, &credentials, config, !disabled, default)
                .await?;
            print_account(&account);
        }
        AccountCommand::List { provider } => {
            for account in database
                .list_accounts(provider.map(ProviderKind::as_str), false)
                .await?
            {
                print_account(&account);
            }
        }
        AccountCommand::SetDefault { account_id } => {
            print_account(&database.set_default_account(account_id).await?);
        }
        AccountCommand::Enable { account_id } => {
            print_account(&database.set_account_enabled(account_id, true).await?);
        }
        AccountCommand::Disable { account_id } => {
            print_account(&database.set_account_enabled(account_id, false).await?);
        }
        AccountCommand::SetConfig { account_id, config } => {
            let config = parse_config(&config)?;
            let account = database
                .find_account(account_id)
                .await?
                .with_context(|| format!("provider account {account_id} was not found"))?;
            let provider = account.provider.parse::<ProviderKind>()?;
            validate_provider_config(provider, &config).map_err(anyhow::Error::msg)?;
            print_account(&database.set_account_config(account_id, config).await?);
        }
        AccountCommand::RotateCredentials {
            account_id,
            credentials_stdin,
        } => {
            let account = database
                .find_account(account_id)
                .await?
                .with_context(|| format!("provider account {account_id} was not found"))?;
            let provider: ProviderKind = account.provider.parse()?;
            let credentials = read_credentials(provider, credentials_stdin)?;
            let version = accounts.rotate_credentials(&account, &credentials).await?;
            println!("rotated credentials for {account_id}; credential_version={version}");
        }
        AccountCommand::Remove { account_id } => {
            database.remove_account(account_id).await?;
            println!("removed provider account {account_id} and its encrypted secret");
        }
    }

    Ok(())
}

fn parse_config(value: &str) -> Result<Value> {
    let parsed: Value = serde_json::from_str(value).context("parse --config JSON")?;
    if !parsed.is_object() {
        bail!("--config must be a JSON object");
    }
    Ok(parsed)
}

fn read_credentials(provider: ProviderKind, from_stdin: bool) -> Result<ProviderCredentials> {
    let credentials = if from_stdin {
        let mut bytes = Zeroizing::new(Vec::new());
        io::stdin()
            .read_to_end(&mut bytes)
            .context("read credentials from stdin")?;
        serde_json::from_slice(&bytes).context("parse credentials JSON from stdin")?
    } else {
        prompt_credentials(provider)?
    };
    if credentials.provider() != provider {
        bail!(
            "credential provider {} does not match requested provider {provider}",
            credentials.provider()
        );
    }
    Ok(credentials)
}

fn prompt_credentials(provider: ProviderKind) -> Result<ProviderCredentials> {
    if provider == ProviderKind::Chatgpt {
        let access_token = rpassword::prompt_password("ChatGPT access token: ")?;
        let account_id = rpassword::prompt_password("ChatGPT account ID: ")?;
        require_non_empty(&access_token, "access token")?;
        require_non_empty(&account_id, "account ID")?;
        return Ok(ProviderCredentials::Chatgpt {
            access_token,
            account_id,
            id_token: String::new(),
            refresh_token: String::new(),
            access_token_expires_at: None,
            refreshed_at: None,
        });
    }

    let api_key = rpassword::prompt_password(format!("{provider} API key: "))?;
    require_non_empty(&api_key, "API key")?;
    Ok(match provider {
        ProviderKind::Openai => ProviderCredentials::Openai { api_key },
        ProviderKind::Fireworks => ProviderCredentials::Fireworks { api_key },
        ProviderKind::Anthropic => ProviderCredentials::Anthropic { api_key },
        ProviderKind::Openrouter => ProviderCredentials::Openrouter { api_key },
        ProviderKind::Deepseek => ProviderCredentials::Deepseek { api_key },
        ProviderKind::Chatgpt => unreachable!("ChatGPT handled above"),
    })
}

fn require_non_empty(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    Ok(())
}

fn print_account(account: &ProviderAccount) {
    println!(
        "{}\t{}\t{}\tenabled={}\tdefault={}",
        account.id, account.provider, account.name, account.enabled, account.is_default
    );
}
