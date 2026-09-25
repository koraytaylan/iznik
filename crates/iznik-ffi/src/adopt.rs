//! Owning a descriptor this process inherited.
//!
//! A pane's master is a number the kernel already opened. Taking ownership of
//! it is the one unsafe step in a session-preserving replacement, and it lives
//! here so every other crate can stay `forbid(unsafe_code)`. The caller has
//! to pass a descriptor that is open and that this process may close.

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixListener;
use std::ptr;
use std::slice;

use portable_pty::{MasterPty, PtySize};

/// What `fcntl` returns when the descriptor is not open.
const CLOSED: i32 = -1;

/// Why an inherited descriptor could not be owned.
#[derive(Debug)]
pub struct AdoptFailure {
    /// What was wrong, in words a log can carry.
    pub detail: String,
}

impl core::fmt::Display for AdoptFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for AdoptFailure {}

impl AdoptFailure {
    /// A descriptor `fcntl` says is not open.
    fn not_open(descriptor: RawFd) -> AdoptFailure {
        AdoptFailure {
            detail: format!("descriptor {descriptor} is not open"),
        }
    }

    /// A descriptor operation failed after the descriptor was known to be open.
    fn operating_system(detail: &str) -> AdoptFailure {
        AdoptFailure {
            detail: format!("{detail}: {}", io::Error::last_os_error()),
        }
    }
}

/// Whether `descriptor` names something the kernel still has open.
///
/// # Errors
///
/// [`AdoptFailure`] when `descriptor` is not open.
fn ensure_open(descriptor: RawFd) -> Result<(), AdoptFailure> {
    // SAFETY: `F_GETFD` reads the close-on-exec flag of `descriptor` and does
    // not close, duplicate, or otherwise consume it. A return of [`CLOSED`]
    // is `EBADF`: the number is not an open descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags == CLOSED {
        return Err(AdoptFailure::not_open(descriptor));
    }
    Ok(())
}

/// Clears close-on-exec so an `exec` keeps `descriptor`.
///
/// # Errors
///
/// [`AdoptFailure`] when `descriptor` is not open or the flag cannot be cleared.
pub fn keep_across_exec(descriptor: RawFd) -> Result<(), AdoptFailure> {
    // SAFETY: as [`ensure_open`]: a read of the flags, and nothing else.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags == CLOSED {
        return Err(AdoptFailure::not_open(descriptor));
    }
    let kept = flags & !libc::FD_CLOEXEC;
    // SAFETY: `F_SETFD` replaces the flags of an open descriptor with `kept`,
    // which is the flags just read without close-on-exec. It does not close it.
    let result = unsafe { libc::fcntl(descriptor, libc::F_SETFD, kept) };
    if result == CLOSED {
        return Err(AdoptFailure::operating_system("clearing close-on-exec"));
    }
    Ok(())
}

/// A second open reference to `descriptor`, itself close-on-exec.
///
/// # Errors
///
/// [`AdoptFailure`] when `descriptor` is not open or cannot be duplicated.
pub fn duplicate_descriptor(descriptor: RawFd) -> Result<OwnedFd, AdoptFailure> {
    ensure_open(descriptor)?;
    // SAFETY: `F_DUPFD_CLOEXEC` duplicates an open descriptor and returns the
    // new number. The minimum number asked for is zero, the first descriptor.
    // The original is left open.
    let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == CLOSED {
        return Err(AdoptFailure::operating_system("duplicating the descriptor"));
    }
    // SAFETY: `duplicate` is the descriptor `fcntl` just opened. This owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

/// The terminal attributes on `descriptor`, or nothing when it has none.
///
/// Adoption does not apply these again: the kernel object already has them.
/// They travel in the record so a later reader can see what was handed over.
#[must_use]
pub fn termios_image(descriptor: RawFd) -> Vec<u8> {
    let mut attributes = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `tcgetattr` writes a `termios` into `attributes` when it
    // succeeds, and writes nothing we read when it fails. `descriptor` is
    // only borrowed for the call.
    let result = unsafe { libc::tcgetattr(descriptor, attributes.as_mut_ptr()) };
    if result == CLOSED {
        return Vec::new();
    }
    // SAFETY: `tcgetattr` returned success, so `attributes` is initialized.
    let attributes = unsafe { attributes.assume_init() };
    let length = size_of::<libc::termios>();
    let pointer = ptr::from_ref(&attributes).cast::<u8>();
    // SAFETY: `pointer` addresses `attributes` for `length` bytes, and
    // `attributes` is alive for this copy.
    unsafe { slice::from_raw_parts(pointer, length) }.to_vec()
}

/// Owns `descriptor` as a pseudoterminal master.
///
/// # Errors
///
/// [`AdoptFailure`] when `descriptor` is not open. The caller must not use
/// the number again: a success closes it when the master is dropped.
pub fn pty_adopt(descriptor: RawFd) -> Result<Box<dyn MasterPty + Send>, AdoptFailure> {
    ensure_open(descriptor)?;
    // SAFETY: [`ensure_open`] just observed the descriptor open, and the
    // caller transfers the obligation to close it. The file takes that.
    let file = unsafe { File::from_raw_fd(descriptor) };
    Ok(Box::new(AdoptedMaster {
        file,
        took_writer: RefCell::new(false),
    }))
}

/// Owns `descriptor` as the daemon's listening socket.
///
/// # Errors
///
/// [`AdoptFailure`] when `descriptor` is not open.
pub fn adopt_listener(descriptor: RawFd) -> Result<UnixListener, AdoptFailure> {
    ensure_open(descriptor)?;
    // SAFETY: as [`pty_adopt`]: the descriptor is open and ownership moves.
    let listener = unsafe { UnixListener::from_raw_fd(descriptor) };
    listener
        .set_nonblocking(true)
        .map_err(|source| AdoptFailure {
            detail: format!("the inherited listener cannot be nonblocking: {source}"),
        })?;
    Ok(listener)
}

/// Owns `descriptor` as an ordinary file, which is how the lock is inherited.
///
/// # Errors
///
/// [`AdoptFailure`] when `descriptor` is not open.
pub fn adopt_file(descriptor: RawFd) -> Result<File, AdoptFailure> {
    ensure_open(descriptor)?;
    // SAFETY: as [`pty_adopt`]: the descriptor is open and ownership moves.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

/// An inherited pseudoterminal master. Dropping it closes the descriptor.
struct AdoptedMaster {
    /// The master, owned.
    file: File,
    /// Whether [`MasterPty::take_writer`] has already been called.
    took_writer: RefCell<bool>,
}

impl MasterPty for AdoptedMaster {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        let mut window = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: size.pixel_width,
            ws_ypixel: size.pixel_height,
        };
        // SAFETY: `TIOCSWINSZ` writes the window size of this open terminal
        // descriptor. `window` is alive for the call and is not retained.
        // `ioctl` is variadic; the pointer is the single extra argument.
        let result = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                libc::TIOCSWINSZ,
                ptr::from_mut(&mut window),
            )
        };
        if result == CLOSED {
            anyhow::bail!("setting the terminal size: {}", io::Error::last_os_error());
        }
        Ok(())
    }

    fn get_size(&self) -> anyhow::Result<PtySize> {
        let mut window = MaybeUninit::<libc::winsize>::uninit();
        // SAFETY: `TIOCGWINSZ` writes a `winsize` into `window` for this open
        // terminal descriptor. On failure the memory is not read.
        let result =
            unsafe { libc::ioctl(self.file.as_raw_fd(), libc::TIOCGWINSZ, window.as_mut_ptr()) };
        if result == CLOSED {
            anyhow::bail!("reading the terminal size: {}", io::Error::last_os_error());
        }
        // SAFETY: the ioctl returned success, so `window` is initialized.
        let window = unsafe { window.assume_init() };
        Ok(PtySize {
            rows: window.ws_row,
            cols: window.ws_col,
            pixel_width: window.ws_xpixel,
            pixel_height: window.ws_ypixel,
        })
    }

    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
        let cloned = self.file.try_clone()?;
        Ok(Box::new(MasterRead { file: cloned }))
    }

    fn take_writer(&self) -> anyhow::Result<Box<dyn Write + Send>> {
        if *self.took_writer.borrow() {
            anyhow::bail!("the writer was already taken");
        }
        let cloned = self.file.try_clone()?;
        *self.took_writer.borrow_mut() = true;
        Ok(Box::new(MasterWrite { file: cloned }))
    }

    fn process_group_leader(&self) -> Option<libc::pid_t> {
        // SAFETY: `tcgetpgrp` reads the foreground group of this open terminal
        // descriptor and does not change it. A non-positive return is none.
        let group = unsafe { libc::tcgetpgrp(self.file.as_raw_fd()) };
        (group > 0).then_some(group)
    }

    fn as_raw_fd(&self) -> Option<RawFd> {
        Some(self.file.as_raw_fd())
    }

    fn tty_name(&self) -> Option<std::path::PathBuf> {
        None
    }
}

/// A read handle on a duplicated master. A closed slave reads as the end.
struct MasterRead {
    /// The duplicate.
    file: File,
}

impl Read for MasterRead {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.file.read(buffer) {
            Err(error) if error.raw_os_error() == Some(libc::EIO) => Ok(0),
            other => other,
        }
    }
}

/// A write handle on a duplicated master.
struct MasterWrite {
    /// The duplicate.
    file: File,
}

impl Write for MasterWrite {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
