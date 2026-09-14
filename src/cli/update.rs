//! `july update`: đối chiếu bản cài đặt hiện tại với bản phát hành ổn định mới nhất.

use super::CliError;
use crate::adapter::AdapterStore;
use crate::update::{
    InstallOwnership, JulyUpdate, ReleaseAsset, UpdateLock, classify_install,
    download_verified_asset, fetch_latest_stable, install_release, plan_july_update, staging_root,
    target_triple,
};
use semver::Version;
use std::path::PathBuf;

pub(crate) async fn run_update() -> Result<(), CliError> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| CliError::Update(format!("build version is not SemVer: {error}")))?;
    let target = target_triple().ok_or_else(|| {
        CliError::Update(format!(
            "July publishes no release asset for {}-{}",
            std::env::consts::ARCH,
            std::env::consts::OS
        ))
    })?;
    let release = fetch_latest_stable().await.map_err(|error| {
        CliError::Update(format!("{error}\nCurrent installation was not changed."))
    })?;
    println!("July Update\n");
    println!("Current July    {current}");
    println!("Latest stable   {}", release.version);
    match plan_july_update(&current, &release, target) {
        JulyUpdate::UpToDate { version } => {
            println!("\nJuly {version} is already up to date.");
            Ok(())
        }
        JulyUpdate::LocalNewer { local, latest } => {
            println!("\nJuly {local} is newer than the latest stable release {latest}.");
            println!("No downgrade was performed.");
            Ok(())
        }
        JulyUpdate::AssetMissing { latest, target } => Err(CliError::Update(format!(
            "release {latest} publishes no asset for {target}"
        ))),
        JulyUpdate::Upgrade { from, to, asset } => {
            println!("Asset           {}", asset.name);
            upgrade(&from, &to, &asset).await
        }
    }
}

async fn upgrade(from: &Version, to: &Version, asset: &ReleaseAsset) -> Result<(), CliError> {
    // File thực thi được giải quyết trước khi tải: July không tải về một bản
    // mà nó vốn không được phép cài.
    let executable = current_executable()?;
    let ownership = classify_install(&executable);
    if ownership == InstallOwnership::Unknown {
        return Err(CliError::Update("July installation ownership is unknown; reinstall with scripts/install.sh. Current installation was not changed.".into()));
    }
    if let InstallOwnership::Managed { manager, command } = ownership {
        return Err(CliError::Update(format!(
            "{} is managed by {manager}; update it with:\n  {command}\nCurrent installation was not changed.",
            executable.display()
        )));
    }
    let store = AdapterStore::open_default()?;
    let staging = staging_root(store.home());
    let lock =
        UpdateLock::acquire(&staging).map_err(|error| CliError::Update(error.to_string()))?;

    println!("\nUpdating July");
    let archive = download_verified_asset(asset, &staging)
        .await
        .map_err(|error| {
            CliError::Update(format!("{error}\nCurrent installation was not changed."))
        })?;
    println!("✓ Downloaded {}", asset.name);
    println!("✓ Verified release");
    install_release(&archive, to, &executable)
        .await
        .map_err(|error| {
            CliError::Update(match error {
                crate::update::InstallError::ReceiptRenewal(_) => error.to_string(),
                _ => format!("{error}\nCurrent installation was not changed."),
            })
        })?;
    println!("✓ Installed July {to} at {}", executable.display());
    println!("\nJuly {from} → {to} is installed.");
    // Binary đã đổi nhưng migration và reconciliation runtime chưa chạy, nên
    // lệnh chưa hoàn thành hợp đồng của `july update`.
    Err(CliError::Update(format!(
        "July {to} was installed, but handoff failed: {}",
        crate::update::handoff::exec(&executable, to, &lock)
    )))
}

pub(crate) fn finalize_update(version: &str, fd: i32) -> Result<(), CliError> {
    if version != env!("CARGO_PKG_VERSION") {
        return Err(CliError::Update(
            "invalid update handoff: binary version mismatch".into(),
        ));
    }
    let store = AdapterStore::open_default()?;
    let _lock = crate::update::handoff::validate(fd, &staging_root(store.home()))
        .map_err(|error| CliError::Update(format!("invalid update handoff: {error}")))?;
    println!("✓ Continuing update with July {version}");
    Err(CliError::UpdateIncomplete)
}

fn current_executable() -> Result<PathBuf, CliError> {
    let executable = std::env::current_exe()
        .map_err(|error| CliError::Update(format!("July cannot locate its own binary: {error}")))?;
    // Symlink được giải quyết để `rename` thay đúng file thật chứ không phải
    // biến một symlink thành bản sao.
    std::fs::canonicalize(&executable).map_err(|error| {
        CliError::Update(format!(
            "July cannot resolve {}: {error}",
            executable.display()
        ))
    })
}
