use xai_grok_version::{
    BuildInfo, full_version, initialize, installed, installed_semver, is_dev_build, version,
};

#[test]
fn injected_identity_drives_all_public_version_views() {
    assert_eq!(version(), "0.0.0-dev");
    assert_eq!(full_version(), "0.0.0-dev");
    assert!(is_dev_build());
    let release = BuildInfo {
        version: "1.2.3-alpha.4",
        full_version: "1.2.3-alpha.4 (abcdef012345)",
        is_dev_build: false,
    };
    initialize(release).unwrap();
    initialize(release).unwrap();
    assert_eq!(version(), release.version);
    assert_eq!(full_version(), release.full_version);
    assert!(!is_dev_build());
    assert_eq!(
        xai_grok_version::display_version(" [alpha]"),
        "1.2.3-alpha.4 [alpha]"
    );
    // This test has its own executable and no other tests that read process environment.
    unsafe { std::env::remove_var(xai_grok_version::TEST_VERSION_ENV) };
    assert_eq!(installed(), release.version);
    assert_eq!(installed_semver().unwrap().to_string(), release.version);
    unsafe { std::env::set_var(xai_grok_version::TEST_VERSION_ENV, " 2.3.4-alpha.5 ") };
    assert_eq!(installed(), "2.3.4-alpha.5");
    assert_eq!(installed_semver().unwrap().to_string(), "2.3.4-alpha.5");
    assert_eq!(version(), release.version);
    assert!(!is_dev_build());
    unsafe { std::env::remove_var(xai_grok_version::TEST_VERSION_ENV) };
}
