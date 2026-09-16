//! Isolated, unattached engine paths shared by headless UI routing fixtures.

use iznik_app::bridge::EngineBridge;
use iznik_client::transport::ClientRuntimePaths;
use std::path::PathBuf;

/// Temporary engine paths, independent of the developer's runtime and SSH files.
pub(crate) struct Directory(PathBuf);

impl Drop for Directory {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

/// Start local engine owners without adding any host or opening a connection.
///
/// # Errors
/// Returns filesystem, runtime or engine startup failures.
pub(crate) fn start(name: &str) -> Result<(EngineBridge, Directory), Box<dyn std::error::Error>> {
    let directory =
        Directory(std::env::temp_dir().join(format!("iznik-ui-{name}-{}", std::process::id())));
    std::fs::create_dir_all(&directory.0)?;
    let artifacts = directory.0.join("artifacts");
    std::fs::create_dir_all(&artifacts)?;
    let bridge = EngineBridge::start(
        artifacts,
        ClientRuntimePaths::under(&directory.0.join("runtime"))?,
    )?;
    Ok((bridge, directory))
}
