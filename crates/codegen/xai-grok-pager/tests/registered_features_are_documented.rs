//! The registry is the source of truth and the operator table is a
//! hand-maintained mirror with no compile-time tripwire of its own. This test is
//! its.

use xai_grok_shell::agent::config::FEATURES;

const CONFIGURATION: &str = include_str!("../docs/user-guide/05-configuration.md");

#[test]
fn every_registered_feature_reaches_the_operator() {
    for spec in FEATURES {
        assert!(
            CONFIGURATION.contains(&format!("`{}`", spec.key)),
            "{} has no row in the 05-configuration.md feature table",
            spec.key,
        );
        assert!(
            CONFIGURATION.contains(&format!("`{}`", spec.env)),
            "{} is undocumented in 05-configuration.md",
            spec.env,
        );
    }
}
