//! Khám phá bản phát hành ổn định của July trên GitHub Releases.
//!
//! Đọc metadata, chọn asset theo SemVer/nền tảng và tải vào staging để xác minh.
//! Việc thay binary và bàn giao tiến trình nằm trong các module con.

pub(crate) mod handoff;
mod install;

pub use install::{
    InstallError, InstallOwnership, UpdateLock, classify_install, install_release, lock_path,
};

use semver::Version;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};
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

/// Trần kích thước asset; một bản July đóng gói nhỏ hơn nhiều.
const ASSET_LIMIT: u64 = 256 << 20;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
    /// SHA-256 GitHub tính khi asset được publish, hex thường không tiền tố.
    pub sha256: String,
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

/// Tên asset là quy ước cố định do `scripts/release.sh` sinh ra, nên việc chọn
/// asset là tra cứu đúng tên chứ không phải đoán theo chuỗi con. Đổi tên ở
/// script thì phải đổi cả ở đây.
pub fn asset_name(version: &Version, target: &str) -> String {
    format!("july-{version}-{target}.tar.gz")
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
            // Tên asset trở thành tên file trong thư mục staging, nên nó là
            // biên tin cậy: metadata release không được điều khiển đường dẫn.
            if name.is_empty()
                || name.contains(['/', '\\'])
                || name.starts_with('.')
                || name.contains("..")
            {
                return Err(format!("asset name {name} is not a plain file name"));
            }
            let sha256 = entry["digest"]
                .as_str()
                .and_then(|digest| digest.strip_prefix("sha256:"))
                .filter(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .ok_or_else(|| format!("asset {name} publishes no sha256 digest"))?
                .to_ascii_lowercase();
            Ok(ReleaseAsset {
                name,
                download_url: download_url.to_owned(),
                sha256,
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

/// Thư mục July giữ bản phát hành đã tải; tách khỏi nơi cài đặt để một lần
/// tải hỏng không bao giờ chạm vào July đang chạy.
pub fn staging_root(home: &Path) -> PathBuf {
    home.join("updates")
}

/// Vì sao một bản phát hành không sẵn sàng để cài.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadError {
    Unreachable(String),
    Http(u16),
    Io(String),
    /// Nội dung tải về không khớp digest GitHub công bố; bản cài đặt hiện tại
    /// không bị đụng tới và file tạm đã bị xoá.
    ChecksumMismatch {
        asset: String,
        expected: String,
        actual: String,
    },
    /// URL không thuộc repository phát hành của July. `parse_release` đã chặn
    /// từ trước; kiểm lại tại đây vì đây mới là nơi thực sự đi ra mạng.
    ForeignUrl(String),
}

impl fmt::Display for DownloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(reason) => {
                write!(formatter, "release download failed: {reason}")
            }
            Self::Http(code) => write!(formatter, "release download returned HTTP {code}"),
            Self::Io(reason) => write!(formatter, "release download could not be stored: {reason}"),
            Self::ChecksumMismatch {
                asset,
                expected,
                actual,
            } => write!(
                formatter,
                "{asset} failed checksum verification: expected sha256 {expected}, got {actual}"
            ),
            Self::ForeignUrl(url) => write!(
                formatter,
                "{url} is not published by {RELEASE_REPOSITORY}; nothing was downloaded"
            ),
        }
    }
}

/// Tải asset vào `staging` rồi xác minh SHA-256 trước khi đặt tên cuối cùng.
///
/// Nội dung tải về nằm ở file `.part` cho tới khi digest khớp, nên không bao
/// giờ có file mang tên asset mà chưa được xác minh. Digest lệch thì file tạm
/// bị xoá và không có gì khác trên máy thay đổi.
pub async fn download_verified_asset(
    asset: &ReleaseAsset,
    staging: &Path,
) -> Result<PathBuf, DownloadError> {
    if !asset.download_url.starts_with(ASSET_URL_PREFIX) {
        return Err(DownloadError::ForeignUrl(asset.download_url.clone()));
    }
    std::fs::create_dir_all(staging).map_err(|error| DownloadError::Io(error.to_string()))?;
    let verified = staging.join(&asset.name);
    // Exclusive private directory: curl cannot follow a preexisting .part symlink.
    use std::os::unix::fs::DirBuilderExt;
    let temporary = staging.join(format!(".download-{}", ulid::Ulid::generate()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&temporary)
        .map_err(|error| DownloadError::Io(error.to_string()))?;
    let partial = temporary.join("asset.part");
    let status = download(&asset.download_url, &partial).await;
    let outcome = match status {
        Err(error) => Err(error),
        Ok(200) => verify_file(&partial, &asset.name, &asset.sha256),
        Ok(code) => Err(DownloadError::Http(code)),
    };
    let outcome = outcome.and_then(|()| {
        std::fs::rename(&partial, &verified).map_err(|error| DownloadError::Io(error.to_string()))
    });
    let _ = std::fs::remove_dir_all(&temporary);
    outcome?;
    Ok(verified)
}

/// So digest thực tế của một file với digest mong đợi; đọc theo dòng chảy nên
/// một asset lớn không phải nằm hết trong bộ nhớ.
pub fn verify_file(path: &Path, asset: &str, expected: &str) -> Result<(), DownloadError> {
    let mut file =
        std::fs::File::open(path).map_err(|error| DownloadError::Io(error.to_string()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|error| DownloadError::Io(error.to_string()))?;
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(DownloadError::ChecksumMismatch {
            asset: asset.to_owned(),
            expected: expected.to_ascii_lowercase(),
            actual,
        });
    }
    Ok(())
}

async fn download(url: &str, destination: &Path) -> Result<u16, DownloadError> {
    let output = tokio::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--max-time",
            "600",
            "--max-filesize",
            &ASSET_LIMIT.to_string(),
            "--user-agent",
            concat!("july/", env!("CARGO_PKG_VERSION")),
            "--write-out",
            "%{http_code}",
            "--output",
        ])
        .arg(destination)
        .arg(url)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| DownloadError::Unreachable(format!("curl could not start: {error}")))?;
    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr);
        let reason = reason.trim();
        return Err(DownloadError::Unreachable(if reason.is_empty() {
            output.status.to_string()
        } else {
            reason.to_owned()
        }));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| DownloadError::Unreachable("download returned no status code".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn release(tag: &str, assets: &[(&str, &str)]) -> String {
        let assets: Vec<Value> = assets
            .iter()
            .map(|(name, url)| {
                serde_json::json!({
                    "name": name,
                    "browser_download_url": url,
                    "digest": format!("sha256:{DIGEST}"),
                })
            })
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
                "july-0.9.0-aarch64-apple-darwin.tar.gz",
                "https://github.com/tranngoc8x/july_workspace/releases/download/v0.9.0/july-0.9.0-aarch64-apple-darwin.tar.gz",
            )],
        );
        let parsed = parse_release(&good).unwrap();
        assert_eq!(parsed.version, Version::new(0, 9, 0));
        assert_eq!(parsed.assets.len(), 1);

        let foreign = release(
            "v0.9.0",
            &[(
                "july-0.9.0-aarch64-apple-darwin.tar.gz",
                "https://example.com/evil.tar.gz",
            )],
        );
        assert_eq!(parsed.assets[0].sha256, DIGEST);
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
    fn assets_without_a_usable_digest_or_plain_name_are_rejected() {
        let url = format!("{ASSET_URL_PREFIX}v0.9.0/july-0.9.0-aarch64-apple-darwin.tar.gz");
        let body = |name: &str, digest: Value| {
            serde_json::json!({
                "tag_name": "v0.9.0",
                "draft": false,
                "prerelease": false,
                "assets": [{ "name": name, "browser_download_url": url, "digest": digest }],
            })
            .to_string()
        };
        let good = "july-0.9.0-aarch64-apple-darwin.tar.gz";
        assert!(parse_release(&body(good, serde_json::json!(format!("sha256:{DIGEST}")))).is_ok());
        // Digest viết hoa vẫn hợp lệ nhưng được chuẩn hoá về chữ thường.
        let upper = body(
            good,
            serde_json::json!(format!("sha256:{}", DIGEST.to_uppercase())),
        );
        assert_eq!(parse_release(&upper).unwrap().assets[0].sha256, DIGEST);
        for digest in [
            serde_json::json!(null),
            serde_json::json!(format!("md5:{DIGEST}")),
            serde_json::json!(DIGEST),
            serde_json::json!("sha256:abc"),
            serde_json::json!(format!("sha256:{}", "z".repeat(64))),
        ] {
            assert!(
                parse_release(&body(good, digest.clone())).is_err(),
                "{digest}"
            );
        }
        for name in [
            "../escape.tar.gz",
            "nested/name.tar.gz",
            ".hidden",
            "a\\b",
            "",
        ] {
            assert!(
                parse_release(&body(name, serde_json::json!(format!("sha256:{DIGEST}")))).is_err(),
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn downloads_refuse_urls_outside_the_release_repository() {
        let scratch = std::env::temp_dir().join(format!("july-update-{}", ulid::Ulid::generate()));
        let staging = staging_root(&scratch);
        let asset = ReleaseAsset {
            name: "july-0.9.0-aarch64-apple-darwin.tar.gz".into(),
            download_url: "https://example.com/july-0.9.0-aarch64-apple-darwin.tar.gz".into(),
            sha256: DIGEST.into(),
        };
        assert!(matches!(
            download_verified_asset(&asset, &staging).await,
            Err(DownloadError::ForeignUrl(_))
        ));
        // Guard chạy trước mọi I/O: không có mạng và không có thư mục nào sinh ra.
        assert!(!scratch.exists());
    }

    #[test]
    fn verification_accepts_only_the_published_digest() {
        let scratch = std::env::temp_dir().join(format!("july-update-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&scratch).unwrap();
        let path = scratch.join("asset.tar.gz");
        // Vector NIST: sha256("abc") = ba7816bf...
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(verify_file(&path, "asset.tar.gz", DIGEST), Ok(()));
        assert_eq!(
            verify_file(&path, "asset.tar.gz", &DIGEST.to_uppercase()),
            Ok(())
        );
        let tampered = "0".repeat(64);
        assert_eq!(
            verify_file(&path, "asset.tar.gz", &tampered),
            Err(DownloadError::ChecksumMismatch {
                asset: "asset.tar.gz".into(),
                expected: tampered,
                actual: DIGEST.into(),
            })
        );
        // Nội dung đổi một byte là digest khác, không phải lỗi đọc file.
        std::fs::write(&path, b"abd").unwrap();
        assert!(matches!(
            verify_file(&path, "asset.tar.gz", DIGEST),
            Err(DownloadError::ChecksumMismatch { .. })
        ));
        assert!(matches!(
            verify_file(&scratch.join("absent"), "absent", DIGEST),
            Err(DownloadError::Io(_))
        ));
        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    fn staging_never_leaves_an_unverified_file_under_the_asset_name() {
        let scratch = std::env::temp_dir().join(format!("july-update-{}", ulid::Ulid::generate()));
        let staging = staging_root(&scratch);
        std::fs::create_dir_all(&staging).unwrap();
        let name = "july-0.9.0-aarch64-apple-darwin.tar.gz";
        let partial = staging.join(format!("{name}.part"));
        std::fs::write(&partial, b"abd").unwrap();
        // Chính là bước download_verified_asset chạy sau khi tải: digest lệch
        // thì file tạm biến mất và tên asset không bao giờ xuất hiện.
        assert!(verify_file(&partial, name, DIGEST).is_err());
        std::fs::remove_file(&partial).unwrap();
        assert!(!staging.join(name).exists());
        assert_eq!(staging, scratch.join("updates"));
        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    fn assets_are_selected_by_exact_platform_name() {
        let target = "aarch64-apple-darwin";
        let parsed = stable("v0.9.0", target);
        assert_eq!(
            select_asset(&parsed, target).unwrap().name,
            "july-0.9.0-aarch64-apple-darwin.tar.gz"
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
