//! Boundary cho danh mục và vòng đời của các ACP adapter cài trên máy.

mod catalog;
mod compatibility;
mod detection;
mod store;

pub use detection::{
    ExecutableSource, ResolvedExecutable, VersionDetection, VersionStatus, probe_version,
    resolve_executable,
};

pub use catalog::{ADAPTERS, AdapterSpec, Installer, Tier, find};
pub use store::{
    AdapterError, AdapterIdentity, AdapterStore, PackageInstaller, SystemInstaller,
    ensure_state_directory, install_command,
};

pub use compatibility::{
    AdapterAction, AdapterPlan, CompatibilityState, InstallationOwnership, plan_adapter,
};
