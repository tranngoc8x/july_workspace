//! Boundary cho danh mục và vòng đời của các ACP adapter cài trên máy,
//! cùng các provider bên ngoài July gọi qua một port của application layer.

mod catalog;
mod compatibility;
mod detection;
mod jev;
mod store;

pub use detection::{
    ExecutableSource, ResolvedExecutable, VersionDetection, VersionStatus, probe_version,
    resolve_executable,
};

pub use catalog::{ADAPTERS, AdapterSpec, Installer, Tier, find};
pub use jev::{JevClient, JevDecisionEngine};
pub use store::{
    AdapterError, AdapterIdentity, AdapterStore, PackageInstaller, SystemInstaller,
    ensure_state_directory, install_command,
};

pub use compatibility::{
    AdapterAction, AdapterPlan, CompatibilityState, InstallationOwnership, plan_adapter,
};
