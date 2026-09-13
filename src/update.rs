//! Khám phá bản phát hành ổn định của July trên GitHub Releases.
//!
//! Module này chỉ lập kế hoạch: nó đọc metadata release, chuẩn hoá tag về
//! SemVer và chọn asset đúng nền tảng. Việc tải, xác minh và thay thế binary
//! thuộc các phần sau của kế hoạch 25, nên ở đây không có mutation cục bộ nào.

use semver::Version;
use serde_json::Value;
use std::fmt;
use std::process::Stdio;

/// Repository duy nhất July chấp nhận làm nguồn cập nhật.
pub const RELEASE_REPOSITORY: &str = "tranngoc8x/july_workspace";

/// GitHub endpoint này đã loại draft và prerelease, nên nó chính là "latest stable".
const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/tranngoc8x/july_workspace/releases/latest";

/// Asset chỉ được phép nằm dưới đúng repository phát hành.
const ASSET_URL_PREFIX: &str = "https://github.com/tranngoc8x/july_workspace/releases/download/";

/// Giới hạn thân phản hồi; metadata release thật nhỏ hơn nhiều.
const RESPONSE_LIMIT: usize = 1 << 20;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Release {
    pub version: Version,
    pub assets: Vec<ReleaseAsset>,
}

/// Hành động dự kiến với binary July. Chưa bao gồm reconciliation runtime,
/// thứ chạy kể cả khi July đã là bản mới nhất.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JulyUpdate {
    UpToDate {
        version: Version,
    },
    Upgrade {
        from: Version,
        to: Version,
        asset: ReleaseAsset,
    },
    /// Bản build cục bộ mới hơn bản phát hành; July không tự hạ cấp.
    LocalNewer {
        local: Version,
        latest: Version,
    },
    AssetMissing {
        latest: Version,
        target: &'static str,
    },
}

/// Bỏ tiền tố `v` của tag rồi so sánh bằng SemVer. Prerelease bị loại: kênh
/// cập nhật MVP chỉ nhận bản ổn định.
pub fn normalize_tag(tag: &str) -> Result<Version, String> {
    let trimmed = tag.trim();
    let number = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let version =
        Version::parse(number).map_err(|error| format!("tag {tag} is not SemVer: {error}"))?;
    if !version.pre.is_empty() {
        return Err(format!("tag {tag} is a prerelease"));
    }
    Ok(version)
}

/// Target triple của máy đang chạy, hoặc `None` khi July không phát hành asset
/// cho nền tảng đó.
pub fn target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        _ => None,
    }
}

/// Tên asset là quy ước cố định, nên việc chọn asset là tra cứu đúng tên chứ
/// không phải đoán theo chuỗi con.
pub fn asset_name(version: &Version, target: &str) -> String {
    format!("july-v{version}-{target}.tar.gz")
}

pub fn select_asset<'a>(release: &'a Release, target: &str) -> Option<&'a ReleaseAsset> {
    let expected = asset_name(&release.version, target);
    release.assets.iter().find(|asset| asset.name == expected)
}

/// Đọc metadata release của GitHub. `draft`/`prerelease` được kiểm lại tại đây
/// thay vì tin vào endpoint, và mọi asset phải nằm dưới repository mong đợi.
pub fn parse_release(body: &str) -> Result<Release, String> {
    let value: Value = serde_json::from_str(body)
        .map_err(|error| format!("release metadata is not JSON: {error}"))?;
    for flag in ["draft", "prerelease"] {
        if value[flag].as_bool().unwrap_or(false) {
            return Err(format!("release is marked {flag}"));
        }
    }
    let tag = value["tag_name"]
        .as_str()
        .ok_or("release metadata has no tag_name")?;
    let version = normalize_tag(tag)?;
    let entries = value["assets"]
        .as_array()
        .ok_or("release metadata has no assets array")?;
    let assets = entries
        .iter()
        .map(|entry| {
            let name = entry["name"]
                .as_str()
                .ok_or("release asset has no name")?
                .to_owned();
            let download_url = entry["browser_download_url"]
                .as_str()
                .ok_or("release asset has no browser_download_url")?;
            if !download_url.starts_with(ASSET_URL_PREFIX) {
                return Err(format!(
                    "asset {name} is not published by {RELEASE_REPOSITORY}"
                ));
            }
            Ok(ReleaseAsset {
                name,
                download_url: download_url.to_owned(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Release { version, assets })
}

/// So sánh phiên bản hiện tại với bản phát hành và chọn asset; thuần tuý nên
/// mọi nhánh đều kiểm thử được không cần mạng.
pub fn plan_july_update(current: &Version, release: &Release, target: &'static str) -> JulyUpdate {
    match current.cmp_precedence(&release.version) {
        std::cmp::Ordering::Equal => JulyUpdate::UpToDate {
            version: current.clone(),
        },
        std::cmp::Ordering::Greater => JulyUpdate::LocalNewer {
            local: current.clone(),
            latest: release.version.clone(),
        },
        std::cmp::Ordering::Less => match select_asset(release, target) {
            Some(asset) => JulyUpdate::Upgrade {
                from: current.clone(),
                to: release.version.clone(),
                asset: asset.clone(),
            },
            None => JulyUpdate::AssetMissing {
                latest: release.version.clone(),
                target,
            },
        },
    }
}

/// Vì sao July không đọc được bản phát hành ổn định.
///
/// Hai nhánh này khác nhau ở chỗ người dùng cần làm gì: không tiếp cận được
/// GitHub thì thử lại sau, còn repository chưa publish thì có chờ cũng vô ích.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReleaseLookupError {
    Unreachable(String),
    Unavailable(String),
}

impl fmt::Display for ReleaseLookupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(reason) => {
                write!(formatter, "Unable to check for July updates: {reason}")
            }
            Self::Unavailable(reason) => formatter.write_str(reason),
        }
    }
}

/// Tải metadata release ổn định mới nhất qua HTTPS.
///
/// `curl` được gọi bằng process args, không qua shell, và `--proto =https`
/// chặn cả redirect rời khỏi HTTPS. July không thêm HTTP client riêng vì toàn
/// bộ nhu cầu mạng của nó là các lệnh explicit do người dùng gọi.
pub async fn fetch_latest_stable() -> Result<Release, ReleaseLookupError> {
    use ReleaseLookupError::{Unavailable, Unreachable};
    let (status, body) = fetch(LATEST_RELEASE_URL).await.map_err(Unreachable)?;
    match status {
        200 => parse_release(&body).map_err(Unavailable),
        // Endpoint này 404 khi repository chưa publish bản ổn định nào; đó là
        // trạng thái hợp lệ chứ không phải lỗi mạng.
        404 => Err(Unavailable(format!(
            "{RELEASE_REPOSITORY} has published no stable release"
        ))),
        code => Err(Unreachable(format!("release lookup returned HTTP {code}"))),
    }
}

/// Trả về mã HTTP cuối cùng kèm thân phản hồi; lỗi chỉ dành cho sự cố truyền tải.
async fn fetch(url: &str) -> Result<(u16, String), String> {
    let output = tokio::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--max-time",
            "30",
            "--max-filesize",
            &RESPONSE_LIMIT.to_string(),
            "--header",
            "Accept: application/vnd.github+json",
            "--user-agent",
            concat!("july/", env!("CARGO_PKG_VERSION")),
            "--write-out",
            "\n%{http_code}",
            url,
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("curl could not start: {error}"))?;
    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr);
        let reason = reason.trim();
        return Err(if reason.is_empty() {
            format!("release lookup failed: {}", output.status)
        } else {
            reason.to_owned()
        });
    }
    let response =
        String::from_utf8(output.stdout).map_err(|_| "release metadata is not UTF-8".to_owned())?;
    let (body, code) = response
        .rsplit_once('\n')
        .ok_or("release lookup returned no status code")?;
    let code = code
        .trim()
        .parse()
        .map_err(|_| format!("release lookup returned an unreadable status code: {code}"))?;
    Ok((code, body.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, assets: &[(&str, &str)]) -> String {
        let assets: Vec<Value> = assets
            .iter()
            .map(|(name, url)| serde_json::json!({ "name": name, "browser_download_url": url }))
            .collect();
        serde_json::json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "assets": assets,
        })
        .to_string()
    }

    fn stable(tag: &str, target: &str) -> Release {
        let version = normalize_tag(tag).unwrap();
        let name = asset_name(&version, target);
        let url = format!("{ASSET_URL_PREFIX}{tag}/{name}");
        parse_release(&release(tag, &[(&name, &url)])).unwrap()
    }

    #[test]
    fn tags_normalize_to_stable_semver_only() {
        for (tag, expected) in [
            ("v0.9.0", "0.9.0"),
            ("0.9.0", "0.9.0"),
            (" v1.2.3 ", "1.2.3"),
        ] {
            assert_eq!(
                normalize_tag(tag).unwrap(),
                Version::parse(expected).unwrap()
            );
        }
        for tag in ["v0.9.0-rc.1", "0.9", "nightly", "vlatest", ""] {
            assert!(normalize_tag(tag).is_err(), "{tag}");
        }
    }

    #[test]
    fn release_metadata_is_rejected_unless_stable_and_repository_owned() {
        let good = release(
            "v0.9.0",
            &[(
                "july-v0.9.0-aarch64-apple-darwin.tar.gz",
                "https://github.com/tranngoc8x/july_workspace/releases/download/v0.9.0/july-v0.9.0-aarch64-apple-darwin.tar.gz",
            )],
        );
        let parsed = parse_release(&good).unwrap();
        assert_eq!(parsed.version, Version::new(0, 9, 0));
        assert_eq!(parsed.assets.len(), 1);

        let foreign = release(
            "v0.9.0",
            &[(
                "july-v0.9.0-aarch64-apple-darwin.tar.gz",
                "https://example.com/evil.tar.gz",
            )],
        );
        assert!(parse_release(&foreign).is_err());
        assert!(parse_release(&release("v0.9.0-rc.1", &[])).is_err());
        assert!(parse_release("not json").is_err());
        for flag in ["draft", "prerelease"] {
            let mut value: Value = serde_json::from_str(&good).unwrap();
            value[flag] = Value::Bool(true);
            assert!(parse_release(&value.to_string()).is_err(), "{flag}");
        }
        assert!(parse_release(&serde_json::json!({ "tag_name": "v0.9.0" }).to_string()).is_err());
    }

    #[test]
    fn assets_are_selected_by_exact_platform_name() {
        let target = "aarch64-apple-darwin";
        let parsed = stable("v0.9.0", target);
        assert_eq!(
            select_asset(&parsed, target).unwrap().name,
            "july-v0.9.0-aarch64-apple-darwin.tar.gz"
        );
        // Asset của nền tảng khác và của phiên bản khác đều không được nhận nhầm.
        assert!(select_asset(&parsed, "x86_64-unknown-linux-gnu").is_none());
        assert!(select_asset(&stable("v0.8.0", target), target).is_some());
        assert!(select_asset(&stable("v0.8.0", target), "aarch64-unknown-linux-gnu").is_none());
    }

    #[test]
    fn planning_upgrades_forward_and_never_downgrades() {
        let target = "aarch64-apple-darwin";
        let latest = stable("v0.9.0", target);
        assert_eq!(
            plan_july_update(&Version::new(0, 8, 0), &latest, target),
            JulyUpdate::Upgrade {
                from: Version::new(0, 8, 0),
                to: Version::new(0, 9, 0),
                asset: latest.assets[0].clone(),
            }
        );
        assert_eq!(
            plan_july_update(&Version::new(0, 9, 0), &latest, target),
            JulyUpdate::UpToDate {
                version: Version::new(0, 9, 0)
            }
        );
        assert_eq!(
            plan_july_update(&Version::new(0, 10, 0), &latest, target),
            JulyUpdate::LocalNewer {
                local: Version::new(0, 10, 0),
                latest: Version::new(0, 9, 0)
            }
        );
        assert_eq!(
            plan_july_update(&Version::new(0, 8, 0), &latest, "x86_64-unknown-linux-gnu"),
            JulyUpdate::AssetMissing {
                latest: Version::new(0, 9, 0),
                target: "x86_64-unknown-linux-gnu"
            }
        );
        // Prerelease cục bộ đứng trước bản ổn định cùng số, nên vẫn là nâng cấp.
        let prerelease = Version::parse("0.9.0-rc.1").unwrap();
        assert!(matches!(
            plan_july_update(&prerelease, &latest, target),
            JulyUpdate::Upgrade { .. }
        ));
    }

    #[test]
    fn lookup_failures_separate_unreachable_from_unpublished() {
        assert_eq!(
            ReleaseLookupError::Unreachable("connection refused".into()).to_string(),
            "Unable to check for July updates: connection refused"
        );
        assert_eq!(
            ReleaseLookupError::Unavailable("nothing published".into()).to_string(),
            "nothing published"
        );
    }

    #[test]
    fn update_endpoints_stay_pinned_to_the_expected_repository() {
        for url in [LATEST_RELEASE_URL, ASSET_URL_PREFIX] {
            assert!(url.starts_with("https://"), "{url}");
            assert!(url.contains(RELEASE_REPOSITORY), "{url}");
        }
    }
}
