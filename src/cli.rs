//! Command-line definitions.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

use crate::domain::{ReleaseChannel, WowClient};

#[derive(Debug, Parser)]
#[command(
    name = "wowaddons",
    version,
    about = "Check and update World of Warcraft addons from the command line",
    propagate_version = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Use this profile instead of the default one
    #[arg(long, global = true, value_name = "NAME")]
    pub profile: Option<String>,

    /// Print one JSON document on stdout
    #[arg(long, global = true)]
    pub json: bool,

    /// More log detail on stderr (repeatable)
    #[arg(short, long, global = true, action = ArgAction::Count, conflicts_with = "quiet")]
    pub verbose: u8,

    /// Print errors only
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Use another config file (also WOWADDONS_CONFIG)
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Never make network requests
    #[arg(long, global = true)]
    pub offline: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create the config file
    Init,
    /// Analyze the AddOns folder in detail
    Scan(InspectArgs),
    /// List installed addons and their tracking status
    List(InspectArgs),
    /// Compare installed addons with compatible remote files
    Check(CheckArgs),
    /// Update addons in one transaction
    Update(UpdateArgs),
    /// Open the folder where ZIP files downloaded by hand go (one per profile)
    Inbox(InboxArgs),
    /// Install the ZIP files of the inbox (offline)
    Import(ImportArgs),
    /// Delete addons from the AddOns folder in one transaction (offline)
    #[command(visible_alias = "uninstall")]
    Remove(RemoveArgs),
    /// Check the configuration, paths and access
    Doctor,
    /// Link an addon folder to a CurseForge project
    Link(LinkArgs),
    /// Remove an explicit link
    Unlink(UnlinkArgs),
    /// List past transactions
    History,
    /// Revert the most recent committed transaction
    Rollback(RollbackArgs),
    /// Read or change global settings
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Manage profiles
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// Store the CurseForge API key in the system secret store
    #[command(subcommand)]
    Auth(AuthCommand),
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Ask for the API key (hidden input), check it, then store it securely
    SetKey {
        /// Read the key from standard input instead of a hidden prompt
        #[arg(long)]
        stdin: bool,
    },
    /// Show where the API key comes from (never its value)
    Status,
    /// Remove the stored API key
    Clear,
}

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// Inspect any folder read-only, without a profile
    #[arg(long, value_name = "PATH")]
    pub addons_path: Option<PathBuf>,
}

#[derive(Debug, Args, Clone, Default)]
pub struct FilterArgs {
    /// Override the client used for the compatibility filter (this run only)
    #[arg(long)]
    pub client: Option<WowClient>,

    /// Limit results to one game version, e.g. 1.60.1
    #[arg(long, value_name = "VERSION")]
    pub game_version: Option<String>,

    /// Explicit CurseForge version type
    #[arg(long, value_name = "ID")]
    pub game_version_type_id: Option<u32>,

    /// Release channel
    #[arg(long)]
    pub channel: Option<ReleaseChannel>,

    /// Limit the run to these addons (repeatable)
    #[arg(long = "addon", value_name = "ADDON")]
    pub addons: Vec<String>,
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    #[command(flatten)]
    pub filter: FilterArgs,

    /// Exit with code 10 if at least one update is available
    #[arg(long)]
    pub fail_on_updates: bool,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    #[command(flatten)]
    pub filter: FilterArgs,

    /// Simulate the run without changing anything
    #[arg(long)]
    pub dry_run: bool,

    /// Skip confirmation prompts (safety checks still apply)
    #[arg(short, long)]
    pub yes: bool,

    /// Include unmanaged addons
    #[arg(long)]
    pub adopt: bool,

    /// Install this exact file (needs exactly one --addon)
    #[arg(long, value_name = "ID")]
    pub file_id: Option<String>,

    /// Allow a file not tagged for the profile's client
    #[arg(long)]
    pub allow_client_mismatch: bool,

    /// Reinstall the file already installed (repair)
    #[arg(long)]
    pub reinstall: bool,
}

#[derive(Debug, Args)]
pub struct InboxArgs {
    /// Only print the folder path; do not open the file manager
    #[arg(long)]
    pub no_open: bool,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Show the plan without changing anything
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the confirmation prompt (safety checks still apply)
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// Addons to remove (name or any of their folders, see `wowaddons list`)
    #[arg(value_name = "ADDON", required = true)]
    pub addons: Vec<String>,

    /// Show the plan without changing anything
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the confirmation prompt (safety checks still apply)
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct LinkArgs {
    /// Addon folder name
    pub addon: String,

    /// CurseForge project ID
    #[arg(long, value_name = "ID")]
    pub project_id: String,
}

#[derive(Debug, Args)]
pub struct UnlinkArgs {
    /// Addon folder name
    pub addon: String,
}

#[derive(Debug, Args)]
pub struct RollbackArgs {
    /// Transaction to revert (default: the most recent committed one)
    pub transaction_id: Option<String>,

    /// Simulate the rollback without changing anything
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the confirmation prompt
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Show every key, its effective value and its source
    List,
    /// Show one key
    Get { key: String },
    /// Change one key
    Set { key: String, value: String },
    /// Remove one key (back to its default)
    Unset { key: String },
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// Add a profile
    Add {
        name: String,
        #[arg(long)]
        client: WowClient,
        #[arg(long, value_name = "PATH")]
        addons_path: PathBuf,
        #[arg(long, value_name = "ID")]
        game_version_type_id: Option<u32>,
    },
    /// List profiles
    List,
    /// Show a profile
    Show { name: String },
    /// Change a profile key
    Set {
        name: String,
        key: String,
        value: String,
        /// Skip the confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
    /// Remove an optional profile key
    Unset { name: String, key: String },
    /// Make a profile the default
    Use { name: String },
    /// Remove a profile (never touches AddOns)
    Remove {
        name: String,
        /// Also delete the profile's state, journals and backups
        #[arg(long)]
        purge: bool,
        /// Skip the confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::Init => "init",
            Command::Scan(_) => "scan",
            Command::List(_) => "list",
            Command::Check(_) => "check",
            Command::Update(_) => "update",
            Command::Inbox(_) => "inbox",
            Command::Import(_) => "import",
            Command::Remove(_) => "remove",
            Command::Doctor => "doctor",
            Command::Link(_) => "link",
            Command::Unlink(_) => "unlink",
            Command::History => "history",
            Command::Rollback(_) => "rollback",
            Command::Config(_) => "config",
            Command::Profile(_) => "profile",
            Command::Auth(_) => "auth",
        }
    }
}
