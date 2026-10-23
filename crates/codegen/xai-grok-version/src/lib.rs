//! Runtime identity of the grok executable, initialized by its composition root.

#![deny(clippy::indexing_slicing)]

use std::sync::OnceLock;

use semver::Version;

pub const TEST_VERSION_ENV: &str = "GROK_TEST_VERSION";

/// All release metadata is injected together before the binary starts work.
/// Keeping it out of library compile-time inputs allows version-only releases to reuse them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: &'static str,
    pub full_version: &'static str,
    pub is_dev_build: bool,
}

// Standalone library tests and auxiliary tools have no shipping binary identity.
// Reading this fallback must not prevent a later composition-root initialization.
static UNINITIALIZED_BUILD: BuildInfo = BuildInfo {
    version: "0.0.0-dev",
    full_version: "0.0.0-dev",
    is_dev_build: true,
};
static BUILD_INFO: OnceLock<BuildInfo> = OnceLock::new();

/// A different identity was already installed in this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlreadyInitialized;

impl std::fmt::Display for AlreadyInitialized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("grok build identity was already initialized with different metadata")
    }
}

impl std::error::Error for AlreadyInitialized {}

/// Install the binary's identity before CLI parsing, configuration, or background work.
/// Repeating the same identity is harmless; a conflicting identity is rejected.
pub fn initialize(info: BuildInfo) -> Result<(), AlreadyInitialized> {
    initialize_in(&BUILD_INFO, info)
}

fn initialize_in(state: &OnceLock<BuildInfo>, info: BuildInfo) -> Result<(), AlreadyInitialized> {
    if *state.get_or_init(|| info) == info {
        Ok(())
    } else {
        Err(AlreadyInitialized)
    }
}

fn build_info_in(state: &OnceLock<BuildInfo>) -> &BuildInfo {
    state.get().unwrap_or(&UNINITIALIZED_BUILD)
}

/// The running executable's plain SemVer, or `0.0.0-dev` for an uninitialized harness.
pub fn version() -> &'static str {
    build_info_in(&BUILD_INFO).version
}

/// True for an unstamped source build or an uninitialized standalone library harness.
pub fn is_dev_build() -> bool {
    build_info_in(&BUILD_INFO).is_dev_build
}

/// The binary's `"<version> (<shortcommit>)"` string, or the standalone development fallback.
pub fn full_version() -> &'static str {
    build_info_in(&BUILD_INFO).full_version
}

/// Returns the [`TEST_VERSION_ENV`] override when set, otherwise [`version`].
/// The env value is trimmed so non-semver-aware callers can pass the result straight into parsing.
pub fn installed() -> String {
    std::env::var(TEST_VERSION_ENV)
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|_| version().to_string())
}

pub fn installed_semver() -> Result<Version, semver::Error> {
    Version::parse(&installed())
}

/// Formats the running version with a channel label, e.g. `"0.2.5 [stable]"`.
/// `channel_label` is pre-formatted by `xai_grok_update::channel_label()`: `" [alpha]"`, `" [stable]"`, or `""` when no pointer is cached.
pub fn display_version(channel_label: &str) -> String {
    format!("{}{}", version(), channel_label)
}

/// Like [`display_version`], but for the full `"0.2.5 (abc1234)"` string.
pub fn display_version_with_commit(version_with_commit: &str, channel_label: &str) -> String {
    format!("{}{}", version_with_commit, channel_label)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: BuildInfo = BuildInfo {
        version: "1.2.3-alpha.4",
        full_version: "1.2.3-alpha.4 (abcdef012345)",
        is_dev_build: false,
    };

    #[test]
    fn fallback_does_not_latch_before_initialization() {
        let state = OnceLock::new();
        assert_eq!(build_info_in(&state), &UNINITIALIZED_BUILD);
        assert!(state.get().is_none());
        initialize_in(&state, RELEASE).unwrap();
        assert_eq!(build_info_in(&state), &RELEASE);
    }

    #[test]
    fn repeated_identity_is_allowed_but_conflicts_are_rejected() {
        let state = OnceLock::new();
        initialize_in(&state, RELEASE).unwrap();
        initialize_in(&state, RELEASE).unwrap();
        for different in [
            BuildInfo {
                version: "1.2.4",
                ..RELEASE
            },
            BuildInfo {
                full_version: "1.2.3-alpha.4 (different)",
                ..RELEASE
            },
            BuildInfo {
                is_dev_build: true,
                ..RELEASE
            },
        ] {
            assert_eq!(initialize_in(&state, different), Err(AlreadyInitialized));
            assert_eq!(build_info_in(&state), &RELEASE);
        }
    }

    #[test]
    fn source_binary_can_supply_a_version_and_remain_a_dev_build() {
        let state = OnceLock::new();
        let source = BuildInfo {
            is_dev_build: true,
            ..RELEASE
        };
        initialize_in(&state, source).unwrap();
        assert_eq!(build_info_in(&state).version, RELEASE.version);
        assert!(build_info_in(&state).is_dev_build);
    }

    #[test]
    fn display_version_formatting_matrix() {
        for (full, label, expected) in [
            ("0.2.5 (abc1234)", " [alpha]", "0.2.5 (abc1234) [alpha]"),
            ("0.2.5 (abc1234)", " [stable]", "0.2.5 (abc1234) [stable]"),
            ("0.2.5 (abc1234)", "", "0.2.5 (abc1234)"),
            (
                "0.1.220-alpha.2 (def0)",
                " [alpha]",
                "0.1.220-alpha.2 (def0) [alpha]",
            ),
        ] {
            assert_eq!(display_version_with_commit(full, label), expected);
        }
        assert_eq!(display_version(""), version());
        assert!(display_version(" [stable]").ends_with("[stable]"));
    }
}
