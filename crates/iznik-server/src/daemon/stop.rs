//! How a daemon is asked to end: `SIGTERM` on Unix, and a forced end on
//! Windows, where the process tree does not deliver that signal.

use std::io;

/// The signal the accept loop waits on.
#[derive(Debug)]
pub struct Termination {
    /// Unix `SIGTERM`.
    #[cfg(unix)]
    inner: tokio::signal::unix::Signal,
    /// Windows Ctrl+C and the close event a console receives.
    #[cfg(windows)]
    inner: tokio::signal::windows::CtrlC,
}

impl Termination {
    /// Installs the handler.
    ///
    /// # Errors
    ///
    /// When the operating system refuses the handler.
    pub fn install() -> io::Result<Termination> {
        #[cfg(unix)]
        {
            let inner = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            Ok(Termination { inner })
        }
        #[cfg(windows)]
        {
            let inner = tokio::signal::windows::ctrl_c()?;
            Ok(Termination { inner })
        }
    }

    /// The next request to end, or nothing when the handler has gone.
    pub async fn recv(&mut self) -> Option<()> {
        self.inner.recv().await
    }
}

/// Asks process `process_id` to end.
///
/// # Errors
///
/// When the operating system refuses, including when no such process exists.
pub async fn end_process(process_id: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        let result = (|| {
            let Ok(raw) = i32::try_from(process_id) else {
                return Err(io::Error::other("the process id does not fit this system"));
            };
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(raw),
                nix::sys::signal::Signal::SIGTERM,
            )
            .map_err(io::Error::other)
        })();
        std::future::ready(result).await
    }
    #[cfg(windows)]
    {
        let status = tokio::process::Command::new("taskkill")
            .args(["/PID", &process_id.to_string(), "/F"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other("the process could not be ended"))
        }
    }
}
