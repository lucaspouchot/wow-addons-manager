//! `wowaddons config …`.

use serde_json::json;

use crate::cli::ConfigCommand;
use crate::config::{ConfigEditor, GLOBAL_KEYS, get_global};
use crate::error::Result;
use crate::output::table;

use super::{Ctx, Outcome};

pub fn run(ctx: &Ctx, cmd: &ConfigCommand) -> Result<Outcome> {
    match cmd {
        ConfigCommand::List => {
            let config = ctx.load_config()?;
            let mut rows = Vec::new();
            let mut items = Vec::new();
            for spec in GLOBAL_KEYS {
                let (value, source) = get_global(&config, spec.key)?;
                rows.push(vec![
                    spec.key.to_string(),
                    value.clone().unwrap_or_else(|| "-".into()),
                    source.to_string(),
                ]);
                items.push(json!({ "key": spec.key, "value": value, "source": source }));
            }
            ctx.out.print(&format!(
                "Config file: {}{}\n\n{}",
                config.path.display(),
                if config.exists {
                    ""
                } else {
                    " (not created yet)"
                },
                table(&["KEY", "VALUE", "SOURCE"], rows)
            ));
            Ok(Outcome::ok(json!({ "config": config.path, "keys": items })))
        }
        ConfigCommand::Get { key } => {
            let config = ctx.load_config()?;
            let (value, source) = get_global(&config, key)?;
            ctx.out.print(value.as_deref().unwrap_or("-"));
            Ok(Outcome::ok(
                json!({ "key": key, "value": value, "source": source }),
            ))
        }
        ConfigCommand::Set { key, value } => {
            let mut editor = ConfigEditor::open(&ctx.paths)?;
            editor.set_global(key, value)?;
            let config = editor.save()?;
            let (v, _) = get_global(&config, key)?;
            ctx.out
                .print(&format!("{key} = {}", v.as_deref().unwrap_or("-")));
            Ok(Outcome::ok(json!({ "key": key, "value": v })))
        }
        ConfigCommand::Unset { key } => {
            let mut editor = ConfigEditor::open(&ctx.paths)?;
            editor.unset_global(key)?;
            let config = editor.save()?;
            let (v, source) = get_global(&config, key)?;
            ctx.out.print(&format!(
                "{key} unset (now {}, {source})",
                v.as_deref().unwrap_or("-")
            ));
            Ok(Outcome::ok(
                json!({ "key": key, "value": v, "source": source }),
            ))
        }
    }
}
