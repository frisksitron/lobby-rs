use std::path::Path;

use xtask::check_deps::{Options, check};

#[test]
fn a_protocol_crate_that_pulls_in_tokio_fails() {
    let manifest =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/protocol-pulls-tokio/Cargo.toml");
    let violations = check(&Options {
        manifest_path: Some(manifest),
    })
    .unwrap();
    assert_eq!(violations.len(), 1, "{violations:#?}");
    assert_eq!(violations[0].krate, "lobby-protocol");
    assert_eq!(violations[0].path, ["lobby-protocol", "innocent", "tokio"]);
    // `axum` is only a dev-dependency there, which is allowed.
}

/// A normal dependency that turns on `automation` is refused; the crate's own dev-dependency on
/// itself is not.
#[test]
fn the_automation_feature_through_a_normal_dependency_fails() {
    let manifest =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/automation-in-release/Cargo.toml");
    let violations = check(&Options {
        manifest_path: Some(manifest),
    })
    .unwrap();
    assert_eq!(violations.len(), 1, "{violations:#?}");
    assert_eq!(violations[0].krate, "installer");
    assert!(
        violations[0].forbidden.contains("automation"),
        "{}",
        violations[0]
    );
}
