//! A fixture: every blocking facility, and the inert ones.
use std::io::Read;
use std::io;
use std::process;
pub fn work() {
    std::thread::sleep(std::time::Duration::from_secs(1));
    let out = std::io::stdout();
    let child = process::Command::new("sh");
    let code: std::process::ExitCode = std::process::ExitCode::SUCCESS;
    let status: Option<std::process::ExitStatus> = None;
    let stream = std::process::Stdio::null();
    writeln!(std::io::stderr(), "through a macro").unwrap();
    let text = format!("{:?}", io::stdout());
    let identifier = std::process::id();
}
