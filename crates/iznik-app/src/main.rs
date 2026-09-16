//! The `iznik-app` binary: one GPUI window holding the crate's themed empty view.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;
use std::rc::Rc;

use gpui_kit::AppContext as _;
use gpui_kit::WindowOptions;
use gpui_kit::component::Root;
use iznik_app::bridge::EngineBridge;
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::transport::ClientRuntimePaths;

/// The flag that asks the program what it takes.
const HELP_FLAG: &str = "--help";
/// The headless bundle writer command.
const BUNDLE_FLAG: &str = "--bundle";

/// The exit code of a command line the binary cannot act on: the only flag
/// it knows is `--help`, and everything else is refused with this.
const USAGE_EXIT_CODE: u8 = 2;
/// Positional argument containing the target triple.
const TARGET_ARGUMENT: usize = 1;
/// Positional argument containing the source binary.
const BINARY_ARGUMENT: usize = 2;
/// Positional argument containing the output directory.
const OUTPUT_ARGUMENT: usize = 3;
/// Positional argument containing the product version.
const VERSION_ARGUMENT: usize = 4;
/// Directory names used for this process's private runtime resources.
const ARTIFACT_DIRECTORY: &str = "iznik-app-artifacts";
/// Directory containing client runtime sockets and state.
const RUNTIME_DIRECTORY: &str = "iznik-app-runtime";

/// Opens one window and runs until the application is asked to stop, or
/// answers `--help` on standard output. A machine with no display is
/// reported at runtime; nothing about linking refuses it.
fn main() -> ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    if arguments.iter().any(|argument| argument == HELP_FLAG) {
        return help();
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == BUNDLE_FLAG)
    {
        return bundle(&arguments);
    }
    if !arguments.is_empty() {
        return usage();
    }
    run()
}

/// The one line `--help` answers with, on standard output, and success — a
/// question asked gets an answer wherever no display exists to refuse it.
fn help() -> ExitCode {
    let mut writing = std::io::stdout();
    if writeln!(
        writing,
        "usage: iznik-app [--help] | --bundle <target> <binary> <output> <version>"
    )
    .and_then(|()| writing.flush())
    .is_err()
    {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Writes a platform bundle without opening a display.
fn bundle(arguments: &[OsString]) -> ExitCode {
    let Some(target) = arguments
        .get(TARGET_ARGUMENT)
        .and_then(|value| value.to_str())
    else {
        return usage();
    };
    let Some(binary) = arguments.get(BINARY_ARGUMENT).map(std::path::PathBuf::from) else {
        return usage();
    };
    let Some(output) = arguments.get(OUTPUT_ARGUMENT).map(std::path::PathBuf::from) else {
        return usage();
    };
    let Some(version) = arguments
        .get(VERSION_ARGUMENT)
        .and_then(|value| value.to_str())
    else {
        return usage();
    };
    let result = if target.contains("darwin") || target.contains("apple") {
        iznik_app::bundle::write_macos(&binary, &output, version)
    } else if target.contains("linux") {
        iznik_app::bundle::write_linux(&binary, &output, version)
    } else {
        return usage();
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _written = writeln!(std::io::stderr(), "iznik-app: {error}");
            ExitCode::FAILURE
        }
    }
}

/// A command line the program does not know: the usage line on standard
/// error, and the refused status.
fn usage() -> ExitCode {
    let _written = writeln!(std::io::stderr(), "usage: iznik-app [--help]");
    ExitCode::from(USAGE_EXIT_CODE)
}

/// Starts the application: one window, the kit's layers initialized, and a
/// window the platform refuses reported with the reason it carried.
fn run() -> ExitCode {
    gpui_kit::application().run(|app| {
        gpui_kit::init(app);
        if let Err(refusal) = iznik_app::theme::apply_default_theme(app) {
            let _written = writeln!(std::io::stderr(), "iznik-app: {refusal}");
        }
        app.spawn(async move |app_context| open_window(app_context))
            .detach();
    });
    ExitCode::SUCCESS
}

/// Opens the one window, from inside the application's own spawn so that the
/// platform is ready for it.
fn open_window(app_context: &mut gpui_kit::AsyncApp) {
    let directory = std::env::temp_dir().join(format!("iznik-app-{}", std::process::id()));
    let artifacts = directory.join(ARTIFACT_DIRECTORY);
    let runtime_directory = directory.join(RUNTIME_DIRECTORY);
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        std::fs::create_dir_all(&artifacts)?;
        let runtime_paths = ClientRuntimePaths::under(&runtime_directory)?;
        let bridge = EngineBridge::start(artifacts, runtime_paths)?;
        let thread = Rc::new(VtThread::start(VtOptions::default())?);
        let window =
            app_context.open_window(WindowOptions::default(), |window, build_context| {
                let shell = build_context.new(|context| {
                    WindowShell::new(
                        bridge,
                        Rc::clone(&thread),
                        ShellOptions::default(),
                        window,
                        context,
                    )
                });
                build_context.new(|root_context| Root::new(shell, window, root_context))
            })?;
        Ok(window)
    })();
    if let Err(refusal) = result {
        let _written = writeln!(std::io::stderr(), "iznik-app: {refusal}");
    }
}
