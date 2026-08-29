//! `FEATURES` is the source of truth and the operator tables are hand-maintained mirrors with no compile-time check of their own.
//! This test is that check.

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
