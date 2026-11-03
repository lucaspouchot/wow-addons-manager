//! Text and JSON output, prompts and progress.
//!
//! Results go to stdout. Logs, warnings, progress and prompts go to stderr.

use std::io::{BufRead, IsTerminal, Write};

use comfy_table::{ContentArrangement, Table, presets};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

use crate::error::{AppError, Result};

#[derive(Clone, Debug)]
pub struct Output {
    pub json: bool,
    pub quiet: bool,
    stderr_tty: bool,
}

impl Output {
    pub fn new(json: bool, quiet: bool) -> Self {
        Output {
            json,
            quiet,
            stderr_tty: std::io::stderr().is_terminal(),
        }
    }

    /// Human-readable result on stdout (nothing in JSON mode).
    pub fn print(&self, text: &str) {
        if self.json {
            return;
        }
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }

    /// Progress message on stderr.
    pub fn status(&self, msg: &str) {
        if !self.quiet && !self.json {
            eprintln!("{msg}");
        }
    }

    pub fn warn(&self, msg: &str) {
        if !self.quiet {
            eprintln!("warning: {msg}");
        }
    }

    /// Ask for confirmation. Without a TTY (or in JSON mode) this is a usage error
    /// unless `assume_yes` is set.
    pub fn confirm(&self, prompt: &str, assume_yes: bool) -> Result<bool> {
        if assume_yes {
            return Ok(true);
        }
        if self.json || !std::io::stdin().is_terminal() {
            return Err(AppError::Usage(
                "confirmation required but no terminal is available; pass --yes".into(),
            ));
        }
        eprint!("{prompt} [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| AppError::Other(format!("cannot read the answer: {e}")))?;
        Ok(matches!(
            line.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }

    /// Read one line from the terminal (used by `init`). `None` without a TTY.
    pub fn ask(&self, prompt: &str) -> Option<String> {
        if self.json || !std::io::stdin().is_terminal() {
            return None;
        }
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).ok()?;
        Some(line.trim().to_string())
    }

    pub fn progress(&self, len: u64, msg: &str) -> ProgressBar {
        let bar = ProgressBar::new(len);
        if self.json || self.quiet || !self.stderr_tty {
            bar.set_draw_target(ProgressDrawTarget::hidden());
        } else {
            bar.set_draw_target(ProgressDrawTarget::stderr());
            if let Ok(style) = ProgressStyle::with_template("{msg} [{bar:30}] {pos}/{len}") {
                bar.set_style(style.progress_chars("=> "));
            }
        }
        bar.set_message(msg.to_string());
        bar
    }
}

/// Plain table without borders.
pub fn table(headers: &[&str], rows: Vec<Vec<String>>) -> String {
    let mut t = Table::new();
    t.load_style(presets::NOTHING);
    t.set_content_arrangement(ContentArrangement::Disabled);
    t.set_header(headers.to_vec());
    for r in rows {
        t.add_row(r);
    }
    t.to_string()
        .lines()
        .map(|l| l.strip_prefix(' ').unwrap_or(l).trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Plural helper: `plural(2, "update")` → "2 updates".
pub fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}
