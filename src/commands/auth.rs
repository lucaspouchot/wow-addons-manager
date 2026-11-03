//! `wowaddons auth set-key | status | clear`.

use std::io::{BufRead, IsTerminal};

use serde_json::json;

use crate::cli::AuthCommand;
use crate::error::{AppError, Result};
use crate::net::{ApiKey, ENV_API_KEY};
use crate::providers::{AddonProvider, ProviderError};
use crate::secrets::{KeySource, os_store, resolve_api_key};

use super::{Ctx, Outcome};

/// Read the key without echoing it, or from stdin with `--stdin`.
/// The key is never accepted as a command-line argument (it would end up in shell history).
fn read_key(from_stdin: bool) -> Result<ApiKey> {
    let raw = if from_stdin {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| AppError::Other(format!("cannot read the key from stdin: {e}")))?;
        line
    } else {
        if !std::io::stdin().is_terminal() {
            return Err(AppError::Usage(
                "no terminal to ask for the key; pipe it with `wowaddons auth set-key --stdin`"
                    .into(),
            ));
        }
        rpassword::prompt_password("CurseForge API key (input hidden): ")
            .map_err(|e| AppError::Other(format!("cannot read the key: {e}")))?
    };
    ApiKey::new(raw).ok_or_else(|| AppError::Usage("the API key is empty".into()))
}

pub async fn run(ctx: &Ctx<'_>, cmd: &AuthCommand) -> Result<Outcome> {
    let store = os_store();
    match cmd {
        AuthCommand::SetKey { stdin } => {
            let key = read_key(*stdin)?;
            let validated = if ctx.global.offline {
                ctx.out
                    .warn("--offline: the key was not checked against CurseForge");
                false
            } else {
                let settings = ctx.load_config()?.settings();
                let provider = ctx.provider_with_key(&settings, key.clone())?;
                match provider.version_types().await {
                    Ok(_) => true,
                    Err(e @ ProviderError::Auth { .. }) => {
                        return Err(AppError::Auth(format!("{e}; the key was not saved")));
                    }
                    Err(e) => {
                        return Err(AppError::Network(format!(
                            "cannot check the key ({e}); it was not saved. \
                             Retry, or save it without checking with --offline"
                        )));
                    }
                }
            };
            store.set(key.expose()).map_err(|e| {
                AppError::Other(format!("cannot save the key in the {}: {e}", store.name()))
            })?;
            ctx.out.print(&format!(
                "API key saved in the {}{}.",
                store.name(),
                if validated {
                    " (accepted by CurseForge)"
                } else {
                    ""
                }
            ));
            if std::env::var_os(ENV_API_KEY).is_some() {
                ctx.out.warn(&format!(
                    "{ENV_API_KEY} is also set and takes precedence over the stored key"
                ));
            }
            Ok(Outcome::ok(
                json!({ "stored_in": store.name(), "validated": validated }),
            ))
        }
        AuthCommand::Status => {
            let resolved = resolve_api_key(store.as_ref());
            if let Some(w) = &resolved.warning {
                ctx.out.warn(w);
            }
            let stored = matches!(store.get(), Ok(Some(_)));
            let source = resolved.key.as_ref().map(|(_, s)| *s);
            let text = match source {
                Some(KeySource::Environment) if stored => format!(
                    "API key: from the environment variable {ENV_API_KEY} \
                     (a key is also stored in the {}, but the environment wins).",
                    store.name()
                ),
                Some(s) => format!("API key: from the {}.", s.describe(store.as_ref())),
                None => "API key: not set. Run `wowaddons auth set-key`.".to_string(),
            };
            ctx.out.print(&text);
            Ok(Outcome::ok(json!({
                "configured": source.is_some(),
                "source": source,
                "stored_in_keyring": stored,
                "store": store.name(),
            })))
        }
        AuthCommand::Clear => {
            let removed = store.delete().map_err(|e| {
                AppError::Other(format!(
                    "cannot remove the key from the {}: {e}",
                    store.name()
                ))
            })?;
            ctx.out.print(if removed {
                "Stored API key removed."
            } else {
                "No API key was stored."
            });
            if std::env::var_os(ENV_API_KEY).is_some() {
                ctx.out
                    .warn(&format!("{ENV_API_KEY} is still set in this environment"));
            }
            Ok(Outcome::ok(json!({ "removed": removed })))
        }
    }
}
