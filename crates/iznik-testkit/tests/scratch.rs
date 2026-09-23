//! Scratch paths are unique to the process and to the call, so two tests
//! running at once — in two processes, or on two threads of one — never share
//! a file.

use std::collections::BTreeSet;
use std::thread;

use iznik_testkit::scratch;

/// Two calls with the same label get two paths, both under the temporary
/// directory, both naming this process and the label.
///
/// # Panics
///
/// When the paths are equal, or not where and what they say.
#[test]
fn scratch_paths_differ_per_call_and_name_the_process() {
    let first = scratch::path("label");
    let second = scratch::path("label");
    assert_ne!(first, second);
    for path in [&first, &second] {
        assert!(path.starts_with(std::env::temp_dir()), "{}", path.display());
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("iznik-label-"), "{name}");
        assert!(
            name.contains(&format!("-{}-", std::process::id())),
            "{name}"
        );
    }
}

/// Paths handed out on many threads at once are all distinct, which is what
/// tests on libtest's threads get.
///
/// # Panics
///
/// When two threads are given the same path.
#[test]
fn scratch_paths_differ_across_threads() {
    let handles: Vec<_> = (0..8)
        .map(|_thread| {
            thread::spawn(|| {
                (0..100)
                    .map(|_call| scratch::path("threads"))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut seen = BTreeSet::new();
    let mut total = 0_usize;
    for handle in handles {
        for path in handle.join().unwrap() {
            total = total.saturating_add(1);
            seen.insert(path);
        }
    }
    assert_eq!(seen.len(), total, "every path is distinct");
}
