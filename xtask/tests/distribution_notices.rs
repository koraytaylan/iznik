//! `THIRD-PARTY-NOTICES`: every package a shipped binary is built from, with
//! its licence, and every licence text once.
//!
//! The cases that read the real graph run `cargo metadata`, which answers
//! from `Cargo.lock` and the registry, fetching the sources a fresh machine
//! has not downloaded yet.

use std::fs;

use xtask::distribution::app::bundle_notices;
use xtask::distribution::notices::{NOTICES, Package, packages, render};
use xtask::distribution::workspace_root;

/// A package for the rendering cases.
fn package(name: &str, licence: &str, texts: &[&str]) -> Package {
    Package {
        name: name.to_owned(),
        version: "1.0.0".to_owned(),
        licence: licence.to_owned(),
        repository: Some(format!("https://example.com/{name}")),
        texts: texts.iter().map(|text| (*text).to_owned()).collect(),
    }
}

/// Every package is listed with its version, licence and repository, and a
/// licence text two packages share is printed once, naming both.
///
/// # Panics
///
/// When a package is missing or a shared text is printed twice.
#[test]
fn notices_list_every_package_and_each_text_once() {
    let listed = [
        package("alpha", "MIT", &["MIT text"]),
        package("beta", "MIT OR Apache-2.0", &["MIT text", "Apache text"]),
        package("gamma", "Zlib", &[]),
    ];
    let said = render("demo", &listed);
    assert!(
        said.contains("alpha 1.0.0 - MIT - https://example.com/alpha"),
        "{said}"
    );
    assert!(said.contains("beta 1.0.0 - MIT OR Apache-2.0 - https://example.com/beta"));
    assert!(said.contains("gamma 1.0.0 - Zlib"));
    assert_eq!(said.matches("MIT text").count(), 1, "{said}");
    assert!(
        said.contains("Carried by: alpha 1.0.0, beta 1.0.0\n"),
        "{said}"
    );
    assert!(said.contains("Carried by: beta 1.0.0\n"), "{said}");
    assert!(said.contains("ship no licence file"), "{said}");
    assert!(said.contains("gamma 1.0.0\n"), "{said}");
}

/// The server's notices for a Linux target hold what it links — the runtime,
/// the pseudoterminal crate, the emulator — and nothing it does not: no
/// workspace package and no development dependency.
///
/// # Panics
///
/// When the graph is wrong, or a package has neither a licence nor a text.
#[test]
fn notices_follow_the_server_graph_for_its_target() {
    let found = packages(
        &workspace_root(),
        "iznik-server",
        "x86_64-unknown-linux-musl",
    )
    .expect("the graph resolves");
    let names: Vec<&str> = found.iter().map(|package| package.name.as_str()).collect();
    for linked in ["tokio", "portable-pty", "libghostty-vt", "zstd"] {
        assert!(names.contains(&linked), "{linked} is linked: {names:?}");
    }
    for absent in [
        "iznik-protocol",
        "iznik-testkit",
        "libtest-mimic",
        "gpui-kit",
        "cbindgen",
    ] {
        assert!(!names.contains(&absent), "{absent} is not in the server");
    }
    for listed in &found {
        assert!(
            listed.licence != "not declared" || !listed.texts.is_empty(),
            "{} has a licence",
            listed.name
        );
    }
    let tokio = found.iter().find(|listed| listed.name == "tokio").unwrap();
    assert!(
        tokio
            .texts
            .iter()
            .any(|text| text.contains("Permission is hereby granted"))
    );
}

/// A bundle's notices go under `Contents/Resources` for macOS and at the root
/// for Linux, and cover the application and the servers it carries.
///
/// # Panics
///
/// When the file is not where the layout keeps it, or misses a package.
#[test]
fn notices_are_written_into_the_bundle() {
    let scratch = iznik_testkit::scratch::path("notices");
    let servers = scratch.join("servers");
    fs::create_dir_all(servers.join("x86_64-unknown-linux-musl")).unwrap();
    fs::write(servers.join("x86_64-unknown-linux-musl/iznik-server"), b"").unwrap();
    let root = workspace_root();
    let darwin = bundle_notices(
        &root,
        "aarch64-apple-darwin",
        &servers,
        &scratch.join("app"),
    )
    .expect("the macOS notices");
    assert_eq!(darwin, scratch.join("app/Contents/Resources").join(NOTICES));
    let linux = scratch.join("linux");
    fs::create_dir_all(&linux).unwrap();
    let written = bundle_notices(&root, "x86_64-unknown-linux-gnu", &servers, &linux)
        .expect("the Linux notices");
    assert_eq!(written, linux.join(NOTICES));
    let said = fs::read_to_string(&written).unwrap();
    assert!(said.contains("gpui-kit "), "the application's packages");
    assert!(said.contains("portable-pty "), "the server's packages");
    let _removed = fs::remove_dir_all(&scratch);
}
