//! `july update`: đối chiếu bản cài đặt hiện tại với bản phát hành ổn định mới nhất.
//!
//! Sau khi binary đã đúng phiên bản, July chạy migration schema rồi đưa các
//! runtime về đúng `AdapterSpec` mà chính bản đang chạy mang theo. Bước này
//! chạy cả khi July đã là bản mới nhất, vì spec có thể đã đổi mà binary thì
//! không.

use super::CliError;
use super::reconcile::reconcile_adapter;
use crate::adapter::{AdapterStore, SystemInstaller};
use crate::storage::SqliteStore;
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
            // Binary không đổi nhưng spec của nó vẫn phải được áp lên runtime.
            reconcile_held(&version, false).await
        }
        JulyUpdate::LocalNewer { local, latest } => {
            println!("\nJuly {local} is newer than the latest stable release {latest}.");
            println!("No downgrade was performed.");
            reconcile_held(&local, false).await
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
    // Migration và reconciliation phải chạy bằng spec của bản mới, nên tiến
    // trình này tự thay chính nó bằng binary vừa cài thay vì chạy tiếp.
    Err(CliError::Update(format!(
        "July {to} was installed, but handoff failed: {}",
        crate::update::handoff::exec(&executable, to, &lock)
    )))
}

/// Nửa sau của update, chạy bằng binary mới ngay sau `handoff::exec`.
pub(crate) async fn finalize_update(version: &str, fd: i32) -> Result<(), CliError> {
    if version != env!("CARGO_PKG_VERSION") {
        return Err(CliError::Update(
            "invalid update handoff: binary version mismatch".into(),
        ));
    }
    let store = AdapterStore::open_default()?;
    let _lock = crate::update::handoff::validate(fd, &staging_root(store.home()))
        .map_err(|error| CliError::Update(format!("invalid update handoff: {error}")))?;
    println!("✓ Continuing update with July {version}");
    let installed = Version::parse(version)
        .map_err(|error| CliError::Update(format!("build version is not SemVer: {error}")))?;
    // Khoá vẫn do fd thừa kế giữ, nên phần này không tự lấy khoá lần nữa.
    reconcile_system(&installed, true).await
}

/// Giữ khoá update trong suốt migration/reconciliation của đường không thay binary.
async fn reconcile_held(installed: &Version, updated: bool) -> Result<(), CliError> {
    let store = AdapterStore::open_default()?;
    let _lock = UpdateLock::acquire(&staging_root(store.home()))
        .map_err(|error| CliError::Update(error.to_string()))?;
    reconcile_system(installed, updated).await
}

/// Áp schema migration và spec runtime của bản July đang chạy.
///
/// Partial failure không được biến thành thành công: adapter hỏng vẫn được báo
/// tên kèm lý do và lệnh thoát khác 0, trong khi phần đã chạy được vẫn giữ.
async fn reconcile_system(installed: &Version, updated: bool) -> Result<(), CliError> {
    let state = if updated {
        format!("July {installed} was installed")
    } else {
        format!("July {installed} is unchanged")
    };

    // Mọi lỗi từ đây trở đi đều phải nói rõ binary đã bị thay hay chưa: sau
    // handoff, người dùng không còn nhìn thấy tiến trình cũ để suy ra điều đó.
    let framed = |reason: String| CliError::Update(format!("{state}, but {reason}"));

    println!("\nMigrating");
    let database = super::database_path()
        .map_err(|error| framed(format!("the workspace database is unreachable: {error}")))?;
    let store = SqliteStore::open(&database)
        .map_err(|error| framed(format!("the workspace database was not migrated: {error}")))?;
    let to = store
        .schema_version()
        .map_err(|error| framed(format!("schema version is unreadable: {error}")))?;
    match store.migrated_from() {
        from if from == to => println!("✓ Workspace schema {to}"),
        from => println!("✓ Workspace schema {from} → {to}"),
    }
    // Store được giữ mở tới hết phần Runtimes: chỉ database mới trả lời được
    // "root này còn agent nào dùng không". Cấu hình do người dùng sở hữu không bị
    // đụng tới; reconciliation chỉ ghi lại danh tính adapter mà July tự quản.

    println!("\nRuntimes");
    let adapters = AdapterStore::open_default()
        .map_err(|error| framed(format!("the adapter store is unreachable: {error}")))?;
    let identities = adapters
        .identities()
        .map_err(|error| framed(format!("recorded adapters are unreadable: {error}")))?;
    if identities.is_empty() {
        println!("(no runtimes are set up; run july setup)");
    }
    let installer = SystemInstaller;
    let search = std::env::var_os("PATH");
    let mut failures = Vec::new();
    for id in identities.keys() {
        let Some(spec) = crate::adapter::find(id) else {
            println!("✗ {id}");
            failures.push(format!("{id}: not supported by July {installed}"));
            continue;
        };
        match reconcile_adapter(spec, &adapters, &installer, search.as_deref(), false).await {
            Ok(report) => {
                match &report.from {
                    Some(from) if report.changed => println!("↑ {id} {from} → {}", report.to),
                    _ => println!("✓ {id} {}", report.to),
                }
                report_housekeeping(super::reconcile::repoint_and_reclaim(
                    spec, &adapters, &report, &store,
                ));
            }
            Err(error) => {
                println!("✗ {id}");
                failures.push(format!("{id}: {error}"));
            }
        }
    }
    drop(store);

    if failures.is_empty() {
        if updated {
            println!("\nJuly {installed} is ready.");
        } else {
            println!("\nSystem is up to date.");
        }
        return Ok(());
    }
    let plural = if failures.len() == 1 {
        "runtime requires"
    } else {
        "runtimes require"
    };
    Err(CliError::Update(format!(
        "{state}.\n{} {plural} attention:\n{}",
        failures.len(),
        failures
            .iter()
            .map(|failure| format!("- {failure}"))
            .collect::<Vec<_>>()
            .join("\n")
    )))
}

/// Dọn dẹp không làm hỏng bản cài, nên chỉ được báo chứ không làm update thất bại.
fn report_housekeeping(housekeeping: crate::cli::reconcile::Housekeeping) {
    if !housekeeping.repointed.is_empty() {
        println!(
            "  ↻ repointed {} agent(s): {}",
            housekeeping.repointed.len(),
            housekeeping.repointed.join(", ")
        );
    }
    if !housekeeping.reclaimed.is_empty() {
        println!(
            "  ⌫ reclaimed {} superseded installation(s)",
            housekeeping.reclaimed.len()
        );
    }
    for warning in housekeeping.warnings {
        println!("  ! {warning}");
    }
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
