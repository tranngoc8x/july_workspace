//! Boundary cho danh mục và vòng đời của các ACP adapter cài trên máy.

mod catalog;
mod store;

pub use catalog::{ADAPTERS, AdapterSpec, Installer, Tier, find};
pub use store::{
    AdapterError, AdapterIdentity, AdapterStore, PackageInstaller, SystemInstaller,
    ensure_state_directory, install_command,
};
