//! Deterministic bundle layout tests: the executable, the metadata, and the
//! servers a bundle carries and the running application finds.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use iznik_app::bundle::{bundled_servers, write_linux, write_macos, write_windows};

/// The permission bits of an executable server.
const EXECUTABLE_MODE: u32 = 0o755;
/// Any execute bit.
const EXECUTE_BITS: u32 = 0o111;

#[test]
/// Linux output contains the executable, every server and a versioned desktop entry.
///
/// # Errors
///
/// Returns fixture I/O failures.
///
/// # Panics
///
/// Panics when generated content differs from the expected layout.
fn linux_layout_is_versioned() -> std::io::Result<()> {
    let root = tempfile_directory("linux")?;
    let binary = root.join("source");
    let servers = distribution(&root)?;
    let output = root.join("linux");
    executable(&binary)?;
    write_linux(&binary, &servers, &output, env!("CARGO_PKG_VERSION"))?;
    assert_eq!(fs::read(output.join("bin/iznik"))?, b"binary");
    assert_ne!(
        fs::metadata(output.join("bin/iznik"))?.permissions().mode() & EXECUTE_BITS,
        0,
        "the application stays executable"
    );
    let desktop = fs::read_to_string(output.join("iznik.desktop"))?;
    assert!(desktop.contains(&format!("Version={}", env!("CARGO_PKG_VERSION"))));
    carried(&output.join("share/iznik/artifacts"))?;
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
/// macOS output contains the executable, every server and a version-stamped property list.
///
/// # Errors
///
/// Returns fixture I/O failures.
///
/// # Panics
///
/// Panics when generated content differs from the expected layout.
fn macos_layout_is_versioned() -> std::io::Result<()> {
    let root = tempfile_directory("macos")?;
    let binary = root.join("source");
    let servers = distribution(&root)?;
    let output = root.join("iznik.app");
    executable(&binary)?;
    write_macos(&binary, &servers, &output, env!("CARGO_PKG_VERSION"))?;
    assert_eq!(fs::read(output.join("Contents/MacOS/iznik"))?, b"binary");
    assert_ne!(
        fs::metadata(output.join("Contents/MacOS/iznik"))?
            .permissions()
            .mode()
            & EXECUTE_BITS,
        0,
        "the application stays executable"
    );
    let plist = fs::read_to_string(output.join("Contents/Info.plist"))?;
    assert!(plist.contains("org.iznik.client"));
    assert!(plist.contains(env!("CARGO_PKG_VERSION")));
    carried(&output.join("Contents/Resources/artifacts"))?;
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
/// Windows output contains `bin/iznik.exe`, every server and the version.
///
/// # Errors
///
/// Returns fixture I/O failures.
///
/// # Panics
///
/// Panics when generated content differs from the expected layout.
fn windows_layout_is_versioned() -> std::io::Result<()> {
    let root = tempfile_directory("windows")?;
    let binary = root.join("source");
    let servers = distribution(&root)?;
    let output = root.join("windows");
    executable(&binary)?;
    write_windows(&binary, &servers, &output, env!("CARGO_PKG_VERSION"))?;
    assert_eq!(fs::read(output.join("bin/iznik.exe"))?, b"binary");
    let version = fs::read_to_string(output.join("version.txt"))?;
    assert!(version.contains(env!("CARGO_PKG_VERSION")));
    carried(&output.join("share/iznik/artifacts"))?;
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
/// A bundle is refused when the servers directory holds no server.
///
/// # Errors
///
/// Returns fixture I/O failures.
///
/// # Panics
///
/// Panics when a bundle without servers is written.
fn a_bundle_without_servers_is_refused() -> std::io::Result<()> {
    let root = tempfile_directory("empty")?;
    let binary = root.join("source");
    let servers = root.join("servers");
    fs::create_dir_all(servers.join("x86_64-unknown-linux-musl"))?;
    fs::write(&binary, b"binary")?;
    let refused = write_linux(
        &binary,
        &servers,
        &root.join("linux"),
        env!("CARGO_PKG_VERSION"),
    );
    assert_eq!(
        refused.map_err(|error| error.kind()),
        Err(std::io::ErrorKind::NotFound)
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
/// The running application finds the servers from its own executable in
/// either layout and in a workspace build's target directory, and finds none
/// beside an executable with none near it.
///
/// # Errors
///
/// Returns fixture I/O failures.
///
/// # Panics
///
/// Panics when a layout's servers are not found, or found where there are none.
fn the_application_finds_its_bundled_servers() -> std::io::Result<()> {
    let root = tempfile_directory("find")?;
    let binary = root.join("source");
    let servers = distribution(&root)?;
    fs::write(&binary, b"binary")?;
    write_linux(
        &binary,
        &servers,
        &root.join("linux"),
        env!("CARGO_PKG_VERSION"),
    )?;
    write_macos(
        &binary,
        &servers,
        &root.join("iznik.app"),
        env!("CARGO_PKG_VERSION"),
    )?;
    assert_eq!(
        bundled_servers(&root.join("linux/bin/iznik")),
        Some(root.join("linux/share/iznik/artifacts"))
    );
    assert_eq!(
        bundled_servers(&root.join("iznik.app/Contents/MacOS/iznik")),
        Some(root.join("iznik.app/Contents/Resources/artifacts"))
    );
    write_windows(
        &binary,
        &servers,
        &root.join("windows"),
        env!("CARGO_PKG_VERSION"),
    )?;
    assert_eq!(
        bundled_servers(&root.join("windows/bin/iznik.exe")),
        Some(root.join("windows/share/iznik/artifacts"))
    );
    assert_eq!(bundled_servers(&root.join("loose/bin/iznik")), None);
    let target = root.join("target");
    fs::create_dir_all(target.join("debug"))?;
    fs::create_dir_all(target.join("distribution"))?;
    assert_eq!(
        bundled_servers(&target.join("debug/iznik-app")),
        None,
        "an empty distribution directory carries nothing"
    );
    fs::rename(
        servers.join("x86_64-unknown-linux-musl"),
        target.join("distribution/x86_64-unknown-linux-musl"),
    )?;
    assert_eq!(
        bundled_servers(&target.join("debug/iznik-app")),
        Some(target.join("distribution")),
        "a workspace build finds what `cargo xtask distribution` wrote"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

/// A staged application executable.
///
/// # Errors
///
/// Returns the write failure.
fn executable(binary: &Path) -> std::io::Result<()> {
    fs::write(binary, b"binary")?;
    fs::set_permissions(binary, fs::Permissions::from_mode(EXECUTABLE_MODE))
}

/// A distribution tree as `xtask distribution` leaves it: two triples, each
/// with an executable server beside its checksums and manifest.
///
/// # Errors
///
/// Returns the write failure.
fn distribution(root: &Path) -> std::io::Result<PathBuf> {
    let servers = root.join("distribution");
    for triple in ["aarch64-apple-darwin", "x86_64-unknown-linux-musl"] {
        let directory = servers.join(triple);
        fs::create_dir_all(&directory)?;
        let server = directory.join("iznik-server");
        fs::write(&server, triple.as_bytes())?;
        fs::set_permissions(&server, fs::Permissions::from_mode(EXECUTABLE_MODE))?;
        fs::write(directory.join("SHA256SUMS"), b"sums")?;
        fs::write(directory.join("manifest.toml"), b"manifest")?;
    }
    Ok(servers)
}

/// Assert that a bundle's servers directory holds both executable servers
/// and nothing of the distribution's checksums or manifests.
///
/// # Errors
///
/// Returns the read failure.
///
/// # Panics
///
/// Panics when a server is missing, not executable, or an extra file came along.
fn carried(artifacts: &Path) -> std::io::Result<()> {
    for triple in ["aarch64-apple-darwin", "x86_64-unknown-linux-musl"] {
        let server = artifacts.join(triple).join("iznik-server");
        assert_eq!(
            fs::read(&server)?,
            triple.as_bytes(),
            "{triple} server is carried byte for byte"
        );
        assert_ne!(
            fs::metadata(&server)?.permissions().mode() & EXECUTE_BITS,
            0,
            "{triple} server stays executable"
        );
        assert!(
            !artifacts.join(triple).join("SHA256SUMS").exists(),
            "checksums are left behind"
        );
        assert!(
            !artifacts.join(triple).join("manifest.toml").exists(),
            "manifests are left behind"
        );
    }
    Ok(())
}

/// A fresh scratch directory for one case.
///
/// # Errors
///
/// Returns the directory creation error.
fn tempfile_directory(label: &str) -> std::io::Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("iznik-bundle-{label}-{}", std::process::id()));
    match fs::remove_dir_all(&path) {
        Ok(()) | Err(_) => {}
    }
    fs::create_dir_all(&path)?;
    Ok(path)
}
