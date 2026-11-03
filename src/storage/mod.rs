//! Paths, locks, state and filesystem helpers. No business decisions here.

pub mod fsutil;
pub mod lock;
pub mod paths;
pub mod state;

pub use lock::FileLock;
pub use paths::AppPaths;
pub use state::{ManagedProject, State};
