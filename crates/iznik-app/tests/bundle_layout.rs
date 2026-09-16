//! Deterministic bundle layout tests.

use std::fs;

use iznik_app::bundle::{write_linux, write_macos};

#[test]
/// Linux output contains the executable and versioned desktop entry.
///
/// # Errors
///
/// Returns fixture I/O failures.
///
/// # Panics
///
/// Panics when generated content differs from the expected layout.
///
fn linux_layout_is_versioned() -> std::io::Result<()> {
    let root = tempfile_directory()?;
    let binary = root.join("source");
    let output = root.join("linux");
    fs::write(&binary, b"binary")?;
    write_linux(&binary, &output, "0.1.0")?;
    assert_eq!(fs::read(output.join("bin/iznik"))?, b"binary");
    let desktop = fs::read_to_string(output.join("iznik.desktop"))?;
    assert!(desktop.contains("Version=0.1.0"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
/// macOS output contains the executable and version-stamped property list.
///
/// # Errors
///
/// Returns fixture I/O failures.
///
/// # Panics
///
/// Panics when generated content differs from the expected layout.
///
fn macos_layout_is_versioned() -> std::io::Result<()> {
    let root = tempfile_directory()?;
    let binary = root.join("source");
    let output = root.join("iznik.app");
    fs::write(&binary, b"binary")?;
    write_macos(&binary, &output, "0.1.0")?;
    assert_eq!(fs::read(output.join("Contents/MacOS/iznik"))?, b"binary");
    let plist = fs::read_to_string(output.join("Contents/Info.plist"))?;
    assert!(plist.contains("org.iznik.client"));
    assert!(plist.contains("0.1.0"));
    fs::remove_dir_all(root)?;
    Ok(())
}

///
/// # Errors
///
/// Returns the directory creation error.
fn tempfile_directory() -> std::io::Result<std::path::PathBuf> {
    let path = std::env::temp_dir().join(format!("iznik-bundle-{}", std::process::id()));
    match fs::remove_dir_all(&path) {
        Ok(()) | Err(_) => {}
    }
    match fs::create_dir_all(&path) {
        Ok(()) => Ok(path),
        Err(error) => Err(error),
    }
}
