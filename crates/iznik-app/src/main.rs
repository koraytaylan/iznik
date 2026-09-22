//! The `iznik-app` binary: one GPUI window holding the crate's themed empty view.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;
use std::rc::Rc;

use gpui_kit::AppContext as _;
use gpui_kit::component::{Root, TitleBar};
use iznik_app::bridge::EngineBridge;
use iznik_app::lifecycle::on_window_closed;
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
/// Positional argument containing the directory of servers to carry.
const SERVERS_ARGUMENT: usize = 5;
/// Directory names used for this process's private runtime resources.
const ARTIFACT_DIRECTORY: &str = "iznik-app-artifacts";
/// Names the directory of servers the application may install on a host,
/// laid out as `<triple>/iznik-server`: the same variable the `iznik` command
/// reads. It overrides the servers a bundle carries, which is how a build run
/// from the workspace is given servers at all.
const ARTIFACTS_VARIABLE: &str = "IZNIK_ARTIFACTS_DIRECTORY";

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
        "usage: iznik-app [--help] | --bundle <target> <binary> <output> <version> <servers>"
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
    let Some(servers) = arguments
        .get(SERVERS_ARGUMENT)
        .map(std::path::PathBuf::from)
    else {
        return usage();
    };
    let result = if target.contains("darwin") || target.contains("apple") {
        iznik_app::bundle::write_macos(&binary, &servers, &output, version)
    } else if target.contains("windows") {
        iznik_app::bundle::write_windows(&binary, &servers, &output, version)
    } else if target.contains("linux") {
        iznik_app::bundle::write_linux(&binary, &servers, &output, version)
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
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|app| {
            gpui_kit::init(app);
            iznik_app::menu::install(app);
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
    // The variable, then the servers the bundle carries, then an empty
    // directory: a build with none still reaches `unix:` sockets and hosts
    // that already run this version.
    let artifacts = std::env::var_os(ARTIFACTS_VARIABLE)
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|executable| iznik_app::bundle::bundled_servers(&executable))
        })
        .unwrap_or_else(|| directory.join(ARTIFACT_DIRECTORY));
    if !artifacts.is_dir() {
        let _written = writeln!(
            std::io::stderr(),
            "iznik-app: no iznik-server builds found, so hosts added over ssh that don't already \
             run iznik will fail to connect. Start the app with ./scripts/app/run.sh (or \
             `cargo app`), which builds the server first."
        );
    }
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        std::fs::create_dir_all(&artifacts)?;
        let runtime_paths = ClientRuntimePaths::resolve()?;
        let bridge = EngineBridge::start(artifacts, runtime_paths)?;
        let thread = Rc::new(VtThread::start(VtOptions::default())?);
        // The person's own ssh configuration, resolved here and not by the
        // shell, so the shell a case builds reads no file this machine holds.
        let ssh_config_path = iznik_app::ssh_config::default_path();
        let options = ShellOptions {
            ssh_config_path,
            ..ShellOptions::default()
        };
        let window =
            app_context.open_window(TitleBar::window_options(), |window, build_context| {
                let shell = build_context.new(|context| {
                    WindowShell::new(bridge, Rc::clone(&thread), options, window, context)
                });
                build_context.new(|root_context| Root::new(shell, window, root_context))
            })?;
        let main_window = window.window_id();
        app_context.update(|app| {
            // Bring the window to the foreground at launch. GPUI opens a
            // window without activating the application, so an app launched
            // from the terminal (and not clicked in the Dock) stays behind
            // the terminal while its window is up.
            app.activate(true);
            on_window_closed(app, main_window, |app| app.quit()).detach();
        });
        Ok(window)
    })();
    if let Err(refusal) = result {
        let _written = writeln!(std::io::stderr(), "iznik-app: {refusal}");
    }
}
