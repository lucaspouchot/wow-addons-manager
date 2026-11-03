//! `wowaddons init`.

use serde_json::json;

use crate::cli::ProfileCommand;
use crate::config::ConfigEditor;
use crate::domain::WowClient;
use crate::error::Result;
use crate::storage::fsutil::atomic_write;

use super::{Ctx, Outcome, profile};

pub fn run(ctx: &Ctx) -> Result<Outcome> {
    let path = &ctx.paths.config_file;
    if path.exists() {
        // Make sure the existing file is valid, but never overwrite it.
        ctx.load_config()?;
        ctx.out
            .print(&format!("Config already exists: {}", path.display()));
        return Ok(Outcome::ok(json!({ "config": path, "created": false })));
    }
    atomic_write(path, ConfigEditor::template().as_bytes())?;
    ctx.out.print(&format!("Created {}", path.display()));

    let mut profile_created = None;
    if let Some(answer) = ctx.out.ask("Create a first profile now? [y/N] ") {
        if matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes") {
            let name = ctx
                .out
                .ask("Profile name (e.g. forever): ")
                .unwrap_or_default();
            let client = ctx
                .out
                .ask(&format!("Client ({}): ", WowClient::known_ids().join(", ")))
                .unwrap_or_default();
            let addons = ctx.out.ask("AddOns folder path: ").unwrap_or_default();
            let client: WowClient = client.parse().map_err(crate::error::AppError::Usage)?;
            let cmd = ProfileCommand::Add {
                name: name.clone(),
                client,
                addons_path: addons.into(),
                game_version_type_id: None,
            };
            profile::run(ctx, &cmd)?;
            profile_created = Some(name);
        }
    } else {
        ctx.out
            .print("Next: wowaddons profile add <name> --client <client> --addons-path <path>");
    }
    Ok(Outcome::ok(json!({
        "config": path,
        "created": true,
        "profile_created": profile_created,
    })))
}
