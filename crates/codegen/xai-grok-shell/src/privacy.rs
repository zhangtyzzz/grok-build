//! Compile-time privacy policy for custom distribution artifacts.
//!
//! This is deliberately a build property rather than another runtime setting:
//! environment variables, managed policy, and remote settings cannot loosen it.

/// Whether this binary was built with the distribution privacy boundary.
pub const fn is_hardened_build() -> bool {
    cfg!(feature = "privacy-hardening")
}

#[cfg(all(test, feature = "privacy-hardening"))]
mod tests {
    use super::*;

    #[test]
    fn distribution_feature_reports_hardened() {
        assert!(is_hardened_build());
    }
}
