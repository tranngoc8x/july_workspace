//! `july update`: đối chiếu bản cài đặt hiện tại với bản phát hành ổn định mới nhất.

use super::CliError;
use crate::update::{JulyUpdate, fetch_latest_stable, plan_july_update, target_triple};
use semver::Version;

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
        // Kế hoạch đã xác định nhưng chưa có bước thực thi, nên lệnh phải thoát
        // với mã lỗi thay vì báo cập nhật thành công.
        JulyUpdate::Upgrade { from, to, asset } => {
            println!("Asset           {}", asset.name);
            println!("\nJuly {from} → {to} is available.");
            Err(CliError::UpdateNotInstallable)
        }
    }
}
