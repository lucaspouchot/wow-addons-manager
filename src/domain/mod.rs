//! Domain types shared by every module.

pub mod addon;
pub mod client;
pub mod profile;
pub mod release;
pub mod status;

pub use addon::{LocalAddon, ProjectRef, ProtectedReason, ProviderKind};
pub use client::WowClient;
pub use profile::{GameProfile, ProfileName};
pub use release::{ReleaseChannel, RemoteFile};
pub use status::{CheckStatus, Tracking};
