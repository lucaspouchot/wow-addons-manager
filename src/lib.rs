//! wowaddons: check and update World of Warcraft addons from the command line.
//!
//! See `spec.md` for the full specification.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

pub mod cancel;
pub mod cli;
pub mod commands;
pub mod config;
pub mod domain;
pub mod error;
pub mod installer;
pub mod net;
pub mod output;
pub mod providers;
pub mod resolver;
pub mod scanner;
pub mod secrets;
pub mod storage;

use std::process::ExitCode;

use clap::Parser;
use serde_json::json;

use crate::cli::{Cli, GlobalArgs};
use crate::error::{AppError, Exit};
use crate::output::Output;

pub const JSON_SCHEMA_VERSION: u32 = 1;

fn init_logging(global: &GlobalArgs) {
    let level = if global.quiet {
        "error"
    } else {
        match global.verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(format!("wow_addons_manager={level}"))
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

/// Entry point used by `main.rs`.
pub fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(e.exit_code().clamp(0, 255) as u8);
        }
    };
    init_logging(&cli.global);
    let out = Output::new(cli.global.json, cli.global.quiet);

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            return ExitCode::from(Exit::General.code());
        }
    };
    runtime.spawn(async {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancel::cancel();
            eprintln!("\nCancelling: finishing the current step, then restoring...");
            if tokio::signal::ctrl_c().await.is_ok() {
                // Second Ctrl+C: stop now. The journal lets the next run recover.
                std::process::exit(Exit::Cancelled.code() as i32);
            }
        }
    });

    let command = cli.command.name();
    let result = runtime.block_on(commands::dispatch(&cli, &out));
    let exit = match result {
        Ok(outcome) => {
            if out.json {
                let ok = matches!(outcome.exit, Exit::Success | Exit::UpdatesAvailable);
                print_json(json!({
                    "schema_version": JSON_SCHEMA_VERSION,
                    "command": command,
                    "ok": ok,
                    "exit_code": outcome.exit.code(),
                    "result": outcome.data,
                }));
            }
            outcome.exit
        }
        Err(e) => {
            report_error(&out, command, &e);
            e.exit()
        }
    };
    runtime.shutdown_background();
    ExitCode::from(exit.code())
}

fn print_json(value: serde_json::Value) {
    match serde_json::to_string_pretty(&value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error: cannot serialize the result: {e}"),
    }
}

fn report_error(out: &Output, command: &str, e: &AppError) {
    if out.json {
        print_json(json!({
            "schema_version": JSON_SCHEMA_VERSION,
            "command": command,
            "ok": false,
            "exit_code": e.exit().code(),
            "error": { "kind": e.kind(), "message": e.to_string(), "hint": e.hint() },
        }));
    }
    eprintln!("error: {e}");
    if let Some(h) = e.hint() {
        eprintln!("  hint: {h}");
    }
}
