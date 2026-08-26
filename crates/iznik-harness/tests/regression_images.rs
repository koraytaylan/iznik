//! The regression images: a tag that is a pure function of a Containerfile's
//! bytes, a missing engine named, and — under podman, ignored by default —
//! a second build that is a no-op, an engine image that is bare and
//! unprivileged, and a host image whose root sshd accepts a connection.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use iznik_harness::deadline::wait_until;
use iznik_harness::images::{
    IMAGE_BUILD_DEADLINE, Images, PROGRAM, ensure_images, ensure_images_with, image_tag,
};
use iznik_harness::process::{self, Completed, Deadline, Output, ProcessError};

/// How long one engine command may take.
const COMMAND_DEADLINE: Duration = Duration::from_mins(1);

/// How long a build that is a no-op may take.
const REBUILD_BOUND: Duration = Duration::from_secs(5);

/// How long the host's sshd may take to accept a connection after start.
const SSHD_READY_CAP: Duration = Duration::from_secs(20);

/// How often readiness is asked.
const SSHD_READY_INTERVAL: Duration = Duration::from_millis(250);

/// The seconds a test container lives at most, whatever happens to the test.
const CONTAINER_LIFETIME: &str = "120";

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    /// A fresh directory named for this process and a purpose.
    ///
    /// # Errors
    ///
    /// When the directory cannot be created.
    fn new(purpose: &str) -> Result<Scratch, std::io::Error> {
        let path = std::env::temp_dir().join(format!(
            "iznik-regression-images-{}-{purpose}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Scratch(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

/// A container removed on drop, whatever the test did.
struct Container(String);

impl Drop for Container {
    fn drop(&mut self) {
        let _removed = podman(&["rm", "--force", &self.0]);
    }
}

/// One engine command, captured, under the command deadline.
///
/// # Errors
///
/// The process runner's error.
fn podman(arguments: &[&str]) -> Result<Completed, ProcessError> {
    let mut command = Command::new(PROGRAM);
    command.args(arguments);
    process::run(command, Deadline(COMMAND_DEADLINE), Output::Capture)
}

/// The images, built when missing.
///
/// # Errors
///
/// The images' error, which names podman when it is not there.
fn images() -> Result<Images, iznik_harness::images::ImagesError> {
    ensure_images(Deadline(IMAGE_BUILD_DEADLINE))
}

/// The standard output of a completed command as text.
fn text(completed: &Completed) -> String {
    String::from_utf8_lossy(&completed.stdout).into_owned()
}

/// `image_tag` is a pure function of a Containerfile's bytes: the same
/// content yields the same tag, one changed byte a different one.
///
/// # Panics
///
/// When equal contents tag differently, a changed byte does not change the
/// tag, or the tag is not sixty-four hex digits.
#[test]
fn regression_images_tag_is_a_pure_function_of_the_bytes() {
    let scratch = Scratch::new("tag").expect("a scratch directory");
    let first = scratch.0.join("Containerfile.first");
    let same = scratch.0.join("Containerfile.same");
    let changed = scratch.0.join("Containerfile.changed");
    std::fs::write(&first, b"FROM scratch\nRUN true\n").expect("writes");
    std::fs::write(&same, b"FROM scratch\nRUN true\n").expect("writes");
    std::fs::write(&changed, b"FROM scratch\nRUN trUe\n").expect("writes");
    let tag = image_tag(&first).expect("tags");
    assert_eq!(
        tag,
        image_tag(&same).expect("tags"),
        "the same bytes, another tag"
    );
    assert_ne!(
        tag,
        image_tag(&changed).expect("tags"),
        "a changed byte, the same tag"
    );
    assert_eq!(tag.len(), 64);
    assert!(
        tag.bytes()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase()),
        "the tag is not lowercase hex, so `sha256sum` would not find it: {tag}"
    );
    assert_eq!(
        tag,
        image_tag(&first).expect("tags"),
        "the tag is not a function of the bytes alone"
    );
}

/// A missing podman produces `ImagesError` naming the program.
///
/// # Panics
///
/// When the images are ensured without an engine, or the error does not
/// name the program.
#[test]
fn regression_images_a_missing_podman_names_the_program() {
    let scratch = Scratch::new("missing").expect("a scratch directory");
    let absent = scratch.0.join("container-engine");
    let error =
        ensure_images_with(&absent, Deadline(COMMAND_DEADLINE)).expect_err("no engine, no images");
    let message = error.to_string();
    assert!(
        message.contains(&absent.display().to_string()),
        "the error does not name the program asked for: {message}"
    );
}

/// Building twice is a no-op the second time: the engine is only asked
/// whether the images exist, never to build, and the call stays under a
/// bound; one image answers to each tag (`podman images --filter reference`
/// is `podman image inspect`'s evidence in list form).
///
/// # Panics
///
/// When podman is missing (the message names it), the second call builds,
/// takes longer than the bound, or a tag names other than one image.
#[test]
#[ignore = "needs podman; run with --run-ignored all"]
fn regression_images_building_twice_builds_nothing_the_second_time() {
    let first =
        images().unwrap_or_else(|error| panic!("`podman` is needed to build the images: {error}"));
    let scratch = Scratch::new("wrapper").expect("a scratch directory");
    let log = scratch.0.join("calls.log");
    let wrapper = scratch.0.join("podman");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec {PROGRAM} \"$@\"\n",
            log.display()
        ),
    )
    .expect("the wrapper is written");
    std::fs::set_permissions(
        &wrapper,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("the wrapper is executable");
    let started = Instant::now();
    let second = ensure_images_with(&wrapper, Deadline(IMAGE_BUILD_DEADLINE))
        .unwrap_or_else(|error| panic!("`podman` is needed to build the images: {error}"));
    let elapsed = started.elapsed();
    assert_eq!(first, second, "the references changed between two calls");
    assert!(elapsed < REBUILD_BOUND, "the second call took {elapsed:?}");
    let calls = std::fs::read_to_string(&log).expect("the wrapper logged");
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "the second call asked the engine more than twice:\n{calls}"
    );
    assert!(
        lines.iter().all(|line| line.starts_with("image exists ")),
        "the second call did more than ask whether the images exist:\n{calls}"
    );
    for reference in [&second.host, &second.engine] {
        let filter = format!("reference={reference}");
        let listed = podman(&["images", "--filter", &filter, "--format", "{{.Id}}"])
            .unwrap_or_else(|error| panic!("`podman images` failed: {error}"));
        let listed = text(&listed);
        let identities: Vec<&str> = listed.lines().collect();
        assert_eq!(identities.len(), 1, "{reference} names {identities:?}");
    }
}

/// The engine image has `ssh` and neither `ssh-agent`, `ssh-add`, `sshd`
/// nor a `~/.ssh` directory, and runs as uid 1000.
///
/// # Panics
///
/// When podman is missing (the message names it), or the image is not as
/// described.
#[test]
#[ignore = "needs podman; run with --run-ignored all"]
fn regression_images_engine_is_bare_and_unprivileged() {
    let images =
        images().unwrap_or_else(|error| panic!("`podman` is needed to build the images: {error}"));
    let script = "command -v ssh; for absent in ssh-agent ssh-add sshd; do command -v \"$absent\" && exit 3; done; test -e \"$HOME/.ssh\" && exit 4; id -u";
    let probe = podman(&[
        "run",
        "--rm",
        "--timeout",
        CONTAINER_LIFETIME,
        &images.engine,
        "sh",
        "-c",
        script,
    ])
    .unwrap_or_else(|error| panic!("`podman run` of the engine image failed: {error}"));
    let output = text(&probe);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines, ["/usr/bin/ssh", "1000"], "{output}");
}

/// The host image has `sshd`, `tic`, `infocmp`, `ps` and a `bash` login
/// shell for uid 1000, and its `sshd` starts as root and accepts a
/// connection.
///
/// # Panics
///
/// When podman is missing (the message names it), a program is absent, the
/// login user is not as described, sshd does not run as root, or no
/// connection is accepted within the cap.
#[test]
#[ignore = "needs podman; run with --run-ignored all"]
fn regression_images_host_runs_a_root_sshd_that_accepts_a_connection() {
    let images =
        images().unwrap_or_else(|error| panic!("`podman` is needed to build the images: {error}"));
    let name = format!("iznik-images-test-{}", std::process::id());
    let container = Container(name.clone());
    podman(&[
        "run",
        "--detach",
        "--rm",
        "--timeout",
        CONTAINER_LIFETIME,
        "--name",
        &name,
        &images.host,
    ])
    .unwrap_or_else(|error| panic!("`podman run` of the host image failed: {error}"));
    let programs = podman(&[
        "exec",
        &name,
        "sh",
        "-c",
        "for program in sshd tic infocmp ps; do command -v \"$program\"; done; getent passwd iznik | cut -d: -f3,7",
    ])
    .unwrap_or_else(|error| panic!("`podman exec` in the host failed: {error}"));
    let listed = text(&programs);
    for expected in [
        "/usr/sbin/sshd",
        "/usr/bin/tic",
        "/usr/bin/infocmp",
        "/usr/bin/ps",
        "1000:/bin/bash",
    ] {
        assert!(
            listed.lines().any(|line| line == expected),
            "{expected} is missing from:\n{listed}"
        );
    }
    let prompt = podman(&["exec", &name, "su", "-", "iznik", "-c", "echo \"[$PS1]\""])
        .unwrap_or_else(|error| panic!("`podman exec` in the host failed: {error}"));
    assert_eq!(
        text(&prompt).trim(),
        "[$ ]",
        "the login shell's prompt is not the predictable bytes"
    );
    let banner = || {
        podman(&[
            "exec",
            &name,
            "bash",
            "-c",
            "exec 3<>/dev/tcp/127.0.0.1/22 && head -c 7 <&3",
        ])
        .map(|completed| text(&completed))
        .is_ok_and(|greeting| greeting.starts_with("SSH-2.0"))
    };
    wait_until(
        SSHD_READY_CAP,
        SSHD_READY_INTERVAL,
        "the host's sshd accepting a connection",
        banner,
    )
    .unwrap_or_else(|error| panic!("the host's sshd never accepted a connection: {error}"));
    let owner = podman(&["exec", &name, "sh", "-c", "ps -o user= -C sshd | head -n 1"])
        .unwrap_or_else(|error| panic!("`podman exec` in the host failed: {error}"));
    assert_eq!(text(&owner).trim(), "root", "sshd does not run as root");
    drop(container);
}
