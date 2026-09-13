//! Pure compatibility checks and recommended actions; performs no runtime mutation.

use super::{AdapterSpec, VersionDetection, VersionStatus};
use semver::{Op, Version, VersionReq};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatibilityState {
    Missing,
    Compatible,
    TooOld,
    Incompatible,
    Broken,
    UnknownVersion,
}

/// Evidence supplied by the caller, never inferred from executable lookup origin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallationOwnership {
    JulyManaged,
    ExternalOrUnknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdapterAction {
    Keep,
    Install { version: &'static str },
    Upgrade { version: &'static str },
    Reinstall { version: &'static str },
    Report,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterPlan<'a> {
    pub detection: Option<&'a VersionDetection>,
    pub state: CompatibilityState,
    pub action: AdapterAction,
}

/// Recommend an action without granting authority to rewrite agent configuration.
/// Callers must prove installation ownership independently of executable resolution.
/// `TooOld` recognizes stable detected versions below a fully specified `>=` bound;
/// that bound may include a prerelease. Excluded detected prereleases are incompatible.
/// Other valid requirement shapes still use `matches`; unmatched versions are reported
/// as incompatible rather than guessing a minimum.
pub fn plan_adapter<'a>(
    spec: &AdapterSpec,
    detection: Option<&'a VersionDetection>,
    ownership: InstallationOwnership,
) -> Result<AdapterPlan<'a>, String> {
    let requirement = VersionReq::parse(spec.version_req)
        .map_err(|error| format!("{}: invalid version requirement: {error}", spec.id))?;
    let pin = Version::parse(spec.install_version)
        .map_err(|error| format!("{}: invalid install version: {error}", spec.id))?;
    if !pin.pre.is_empty() || !requirement.matches(&pin) {
        return Err(format!(
            "{}: install version must be stable and satisfy its requirement",
            spec.id
        ));
    }
    let state = match detection.map(|detected| &detected.status) {
        None => CompatibilityState::Missing,
        Some(VersionStatus::UnknownVersion) => CompatibilityState::UnknownVersion,
        Some(VersionStatus::Broken(_)) => CompatibilityState::Broken,
        Some(VersionStatus::Detected(version)) if requirement.matches(version) => {
            CompatibilityState::Compatible
        }
        Some(VersionStatus::Detected(version))
            if version.pre.is_empty()
                && requirement.comparators.iter().any(|bound| {
                    if bound.op != Op::GreaterEq {
                        return false;
                    }
                    let (Some(minor), Some(patch)) = (bound.minor, bound.patch) else {
                        return false;
                    };
                    let mut minimum = Version::new(bound.major, minor, patch);
                    minimum.pre = bound.pre.clone();
                    version.cmp_precedence(&minimum).is_lt()
                }) =>
        {
            CompatibilityState::TooOld
        }
        Some(VersionStatus::Detected(_)) => CompatibilityState::Incompatible,
    };
    let version = spec.install_version;
    let action = match state {
        CompatibilityState::Missing => AdapterAction::Install { version },
        CompatibilityState::Compatible => AdapterAction::Keep,
        CompatibilityState::TooOld if ownership == InstallationOwnership::JulyManaged => {
            AdapterAction::Upgrade { version }
        }
        CompatibilityState::Broken if ownership == InstallationOwnership::JulyManaged => {
            AdapterAction::Reinstall { version }
        }
        _ => AdapterAction::Report,
    };
    Ok(AdapterPlan {
        detection,
        state,
        action,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{ExecutableSource, ResolvedExecutable, VersionStatus, find};
    use semver::Version;

    fn detected(version: &str) -> VersionDetection {
        VersionDetection {
            executable: ResolvedExecutable {
                path: "/custom/chosen-adapter".into(),
                source: ExecutableSource::Explicit,
            },
            status: VersionStatus::Detected(Version::parse(version).unwrap()),
        }
    }

    #[test]
    fn classifies_requirement_boundaries_not_install_pin() {
        use CompatibilityState::*;
        let spec = AdapterSpec {
            install_version: "1.15.0",
            ..*find("codex").unwrap()
        };
        for (version, state, action) in [
            (
                "1.6.2",
                TooOld,
                AdapterAction::Upgrade { version: "1.15.0" },
            ),
            (
                "1.9.99+build.2",
                TooOld,
                AdapterAction::Upgrade { version: "1.15.0" },
            ),
            ("1.10.0+build.1", Compatible, AdapterAction::Keep),
            ("1.12.0", Compatible, AdapterAction::Keep),
            ("1.99.0", Compatible, AdapterAction::Keep),
            ("2.0.0", Incompatible, AdapterAction::Report),
            ("1.9.0-rc.1", Incompatible, AdapterAction::Report),
            ("1.12.0-beta.1", Incompatible, AdapterAction::Report),
        ] {
            let detection = detected(version);
            let plan =
                plan_adapter(&spec, Some(&detection), InstallationOwnership::JulyManaged).unwrap();
            assert_eq!((plan.state, plan.action), (state, action), "{version}");
            assert!(std::ptr::eq(plan.detection.unwrap(), &detection));
        }
        for (id, old, accepted, next) in [
            ("claude", "0.69.9", "0.70.1", "0.71.0"),
            ("claude-rust", "0.1.21", "0.1.23", "0.2.0"),
            ("deepseek", "0.4.25", "0.4.27", "0.5.0"),
        ] {
            for (version, state) in [(old, TooOld), (accepted, Compatible), (next, Incompatible)] {
                assert_eq!(
                    plan_adapter(
                        find(id).unwrap(),
                        Some(&detected(version)),
                        InstallationOwnership::JulyManaged
                    )
                    .unwrap()
                    .state,
                    state,
                    "{id} {version}"
                );
            }
        }
    }

    #[test]
    fn mutation_recommendations_require_ownership_not_resolution_origin() {
        let spec = find("codex").unwrap();
        let missing = plan_adapter(spec, None, InstallationOwnership::ExternalOrUnknown).unwrap();
        assert_eq!(
            (missing.state, missing.action),
            (
                CompatibilityState::Missing,
                AdapterAction::Install { version: "1.10.0" }
            )
        );
        for source in [
            ExecutableSource::Explicit,
            ExecutableSource::Managed,
            ExecutableSource::Path,
        ] {
            for ownership in [
                InstallationOwnership::JulyManaged,
                InstallationOwnership::ExternalOrUnknown,
            ] {
                for (status, state, managed_action) in [
                    (
                        VersionStatus::Broken("permission denied".into()),
                        CompatibilityState::Broken,
                        AdapterAction::Reinstall { version: "1.10.0" },
                    ),
                    (
                        VersionStatus::Detected(Version::new(1, 6, 2)),
                        CompatibilityState::TooOld,
                        AdapterAction::Upgrade { version: "1.10.0" },
                    ),
                    (
                        VersionStatus::UnknownVersion,
                        CompatibilityState::UnknownVersion,
                        AdapterAction::Report,
                    ),
                ] {
                    let detection = VersionDetection {
                        executable: ResolvedExecutable {
                            source,
                            ..detected("1.6.2").executable
                        },
                        status,
                    };
                    let plan = plan_adapter(spec, Some(&detection), ownership).unwrap();
                    let action = if ownership == InstallationOwnership::JulyManaged {
                        managed_action
                    } else {
                        AdapterAction::Report
                    };
                    assert_eq!((plan.state, plan.action), (state, action));
                    assert_eq!(plan.detection.unwrap(), &detection);
                }
            }
        }
    }

    #[test]
    fn matches_remains_authoritative_for_other_valid_requirements() {
        for (req, version, state) in [
            ("^1.10", "1.12.0", CompatibilityState::Compatible),
            ("^1.10", "1.6.2", CompatibilityState::Incompatible),
            (
                ">=1.10.0-rc.1, <2.0.0",
                "1.10.0-rc.2",
                CompatibilityState::Compatible,
            ),
            (">=1.10.0-rc.1, <2.0.0", "1.9.0", CompatibilityState::TooOld),
            (
                ">=1.10.0, >=1.12.0, <2.0.0",
                "1.11.0",
                CompatibilityState::TooOld,
            ),
        ] {
            let spec = AdapterSpec {
                version_req: req,
                install_version: "1.15.0",
                ..*find("codex").unwrap()
            };
            assert_eq!(
                plan_adapter(
                    &spec,
                    Some(&detected(version)),
                    InstallationOwnership::JulyManaged
                )
                .unwrap()
                .state,
                state
            );
        }
    }

    #[test]
    fn invalid_catalog_never_produces_an_install_plan() {
        for (req, pin) in [
            ("nonsense", "1.10.0"),
            (">=1.10.0, <2.0.0", "bad"),
            (">=1.10.0, <2.0.0", "2.0.0"),
            (">=1.10.0-rc.1", "1.10.0-rc.2"),
        ] {
            let spec = AdapterSpec {
                version_req: req,
                install_version: pin,
                ..*find("codex").unwrap()
            };
            assert!(
                plan_adapter(&spec, None, InstallationOwnership::JulyManaged).is_err(),
                "{req} {pin}"
            );
        }
    }
}
