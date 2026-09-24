//! Signing a macOS bundle: without an identity it is signed ad hoc, the macOS
//! servers it carries under `Contents/Resources` are signed on their own, and
//! the whole bundle verifies strictly. Runs where `codesign` is — macOS — and
//! passes trivially elsewhere, since there is nothing to sign with.

use std::fs;
use std::path::Path;
use std::process::Command;

use xtask::distribution::signing::{Signature, sign_bundle};

/// The property list the application's bundle writer writes, in its shape.
const PROPERTY_LIST: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist><dict><key>CFBundleIdentifier</key><string>dev.iznik.test</string><key>CFBundleShortVersionString</key><string>0.0.0</string><key>CFBundleName</key><string>iznik</string><key>CFBundleExecutable</key><string>iznik</string><key>CFBundlePackageType</key><string>APPL</string><key>NSHighResolutionCapable</key><true/></dict></plist>\n";

/// A Mach-O executable every macOS machine has, standing in for the
/// application and its macOS server.
const STAND_IN: &str = "/bin/echo";

/// What `codesign --display` says about `path`, both streams, or why it
/// could not be asked.
fn displayed(path: &Path) -> String {
    Command::new("codesign")
        .args(["--display", "--verbose=2"])
        .arg(path)
        .output()
        .map_or_else(
            |error| error.to_string(),
            |shown| {
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&shown.stdout),
                    String::from_utf8_lossy(&shown.stderr)
                )
            },
        )
}

/// A bundle laid out as the application's writer lays it out, signed ad hoc,
/// verifies strictly, and its macOS server carries an ad hoc signature of
/// its own.
///
/// # Panics
///
/// When signing or verification fails, or the server is left unsigned.
#[test]
fn signing_seals_a_bundle_ad_hoc() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let scratch = iznik_testkit::scratch::path("signing");
    let bundle = scratch.join("iznik.app");
    let contents = bundle.join("Contents");
    fs::create_dir_all(contents.join("MacOS")).unwrap();
    fs::copy(STAND_IN, contents.join("MacOS/iznik")).unwrap();
    fs::write(contents.join("Info.plist"), PROPERTY_LIST).unwrap();
    let artifacts = contents.join("Resources/artifacts");
    let darwin = artifacts.join("aarch64-apple-darwin");
    let linux = artifacts.join("x86_64-unknown-linux-musl");
    fs::create_dir_all(&darwin).unwrap();
    fs::create_dir_all(&linux).unwrap();
    fs::copy(STAND_IN, darwin.join("iznik-server")).unwrap();
    fs::write(linux.join("iznik-server"), b"\x7fELF not a Mach-O").unwrap();
    fs::write(contents.join("Resources/THIRD-PARTY-NOTICES"), b"notices").unwrap();

    sign_bundle(&bundle, "aarch64-apple-darwin", &Signature::AdHoc).expect("signed ad hoc");

    let verified = Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&bundle)
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "the bundle verifies: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
    assert!(
        displayed(&bundle).contains("Signature=adhoc"),
        "the bundle is signed ad hoc"
    );
    assert!(
        displayed(&darwin.join("iznik-server")).contains("Signature=adhoc"),
        "the macOS server is signed on its own"
    );
    let _removed = fs::remove_dir_all(&scratch);
}

/// An identity is used as named; none, an empty one or `-` signs ad hoc.
///
/// # Panics
///
/// When the signature read is another.
#[test]
fn signing_reads_the_identity_as_named() {
    assert_eq!(
        Signature::named(Some("Developer ID Application: Example (TEAM)")),
        Signature::Identity("Developer ID Application: Example (TEAM)".to_owned())
    );
    for ad_hoc in [None, Some(""), Some("  "), Some("-")] {
        assert_eq!(Signature::named(ad_hoc), Signature::AdHoc, "{ad_hoc:?}");
    }
}
