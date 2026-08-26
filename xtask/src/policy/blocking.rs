//! No blocking standard-library facility under the two asynchronous crates:
//! `iznik-server` and `iznik-client` are asynchronous end to end, and in their
//! sources nothing names `std::thread::sleep`, the blocking `std::io` streams
//! and traits, or `std::process` beyond its inert types and values
//! (`ExitCode`, `ExitStatus`, `Stdio`, `Output`, `id`) — the runtime's
//! equivalents are used instead. One file is the exception and part of the
//! rule: `crates/iznik-server/src/pty/streams.rs`, whose whole purpose is to
//! turn a pseudoterminal's blocking descriptor into async streams on
//! dedicated threads.
//!
//! `std::io` is held to a list, because its error types are inert and every
//! async interface returns them; `std::process` is held to an allowlist, as
//! `CONTRIBUTING.md` section 3.6 words it. Resolution is syntactic: a name
//! re-exported through the exception file and used under that path is
//! invisible to it, and review is what catches that.

use std::collections::BTreeMap;
use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};
use syn::visit::{self, Visit};

use super::{PolicyError, Violation, files_under, line_of, parse_rust, relative};

/// The crates held to the rule, relative to the root.
const ASYNCHRONOUS_SOURCES: &[&str] = &["crates/iznik-server/src", "crates/iznik-client/src"];

/// The one file the rule does not apply to, relative to the root.
const EXCEPTION: &str = "crates/iznik-server/src/pty/streams.rs";

/// The facilities outside `std::process` that block: the architecture's
/// list, and the rest of `std::io`'s reading and writing.
const FORBIDDEN: &[&str] = &[
    "std::thread::sleep",
    "std::io::Read",
    "std::io::Write",
    "std::io::BufRead",
    "std::io::Seek",
    "std::io::copy",
    "std::io::read_to_string",
    "std::io::stdin",
    "std::io::stdout",
    "std::io::stderr",
    "std::io::Stdin",
    "std::io::Stdout",
    "std::io::Stderr",
    "std::io::BufReader",
    "std::io::BufWriter",
    "std::io::LineWriter",
    "std::io::prelude",
];

/// The module every item of which blocks or bypasses the runtime, except the
/// names below.
const PROCESS_MODULE: &str = "std::process";

/// The inert names of `std::process`: types a binary returns and
/// `tokio::process` hands out, and the process id.
const ALLOWED_PROCESS_ITEMS: &[&str] = &["ExitCode", "ExitStatus", "Stdio", "Output", "id"];

/// The modules a glob import of which cannot be told apart from importing a
/// forbidden facility.
const FORBIDDEN_GLOBS: &[&str] = &["std::thread", "std::io", "std::io::prelude", "std::process"];

/// The rule a blocking facility breaks.
const RULE: &str = "blocking";

/// The forbidden facility a path from `std` names or lies inside, if any.
fn forbidden_facility(path: &str) -> Option<String> {
    if let Some(rest) = path.strip_prefix(PROCESS_MODULE) {
        let item = rest.strip_prefix("::")?.split("::").next()?;
        return (!ALLOWED_PROCESS_ITEMS.contains(&item))
            .then(|| format!("{PROCESS_MODULE}::{item}"));
    }
    FORBIDDEN
        .iter()
        .find(|facility| {
            path == **facility
                || path
                    .strip_prefix(*facility)
                    .is_some_and(|rest| rest.starts_with("::"))
        })
        .map(|facility| (*facility).to_owned())
}

/// The paths a file names: its `use` declarations, flattened to local names,
/// and every path in its items resolved through them.
#[derive(Debug, Default)]
struct Paths {
    /// Local name to the full path it imports.
    imports: BTreeMap<String, String>,
    /// Every path named, as `std::…` when it resolves to `std`, with its line.
    named: Vec<(String, usize)>,
    /// Glob imports of forbidden modules, with their lines.
    globs: Vec<(String, usize)>,
}

/// A run of `ident::ident::…` tokens being read out of a macro invocation.
#[derive(Debug, Default)]
struct PathRun {
    /// The segments so far.
    segments: Vec<String>,
    /// The line of the first segment.
    line: usize,
    /// Whether the last token was the first `:` of a `::`.
    half_separator: bool,
    /// Whether a `::` was just completed, so the next identifier extends the
    /// run rather than starting another.
    after_separator: bool,
}

impl Paths {
    /// Flattens one `use` tree under the prefix it hangs from.
    fn record_use_tree(&mut self, tree: &syn::UseTree, prefix: &str) {
        match tree {
            syn::UseTree::Path(path) => {
                let extended = join(prefix, &path.ident.to_string());
                self.record_use_tree(&path.tree, &extended);
            }
            syn::UseTree::Name(name) => {
                let identifier = name.ident.to_string();
                let full = if identifier == "self" {
                    prefix.to_owned()
                } else {
                    join(prefix, &identifier)
                };
                let local = full.rsplit("::").next().unwrap_or(&full).to_owned();
                self.named.push((full.clone(), line_of(&name.ident)));
                self.imports.insert(local, full);
            }
            syn::UseTree::Rename(rename) => {
                let identifier = rename.ident.to_string();
                let full = if identifier == "self" {
                    prefix.to_owned()
                } else {
                    join(prefix, &identifier)
                };
                self.named.push((full.clone(), line_of(&rename.ident)));
                self.imports.insert(rename.rename.to_string(), full);
            }
            syn::UseTree::Glob(glob) => {
                if FORBIDDEN_GLOBS.contains(&prefix) {
                    self.globs
                        .push((prefix.to_owned(), line_of(&glob.star_token)));
                }
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.record_use_tree(item, prefix);
                }
            }
        }
    }

    /// Records every path-shaped run of tokens handed to a macro — a call
    /// such as `writeln!(std::io::stderr(), …)` names a facility syn does not
    /// parse as a path — groups included. `::` arrives as two `:` tokens.
    fn record_macro_tokens(&mut self, tokens: TokenStream) {
        let mut run = PathRun::default();
        for token in tokens {
            match token {
                TokenTree::Ident(identifier) => {
                    if !run.after_separator {
                        self.record_run(&std::mem::take(&mut run));
                        run.line = line_of(&identifier);
                    }
                    run.segments.push(identifier.to_string());
                    run.after_separator = false;
                    run.half_separator = false;
                }
                TokenTree::Punct(punctuation) if punctuation.as_char() == ':' => {
                    if run.half_separator {
                        run.after_separator = !run.segments.is_empty();
                        run.half_separator = false;
                    } else {
                        run.half_separator = true;
                        run.after_separator = false;
                    }
                }
                other => {
                    self.record_run(&std::mem::take(&mut run));
                    if let TokenTree::Group(group) = other {
                        self.record_macro_tokens(group.stream());
                    }
                }
            }
        }
        self.record_run(&run);
    }

    /// Records one run of segments, when it resolves to `std`.
    fn record_run(&mut self, run: &PathRun) {
        if let Some(resolved) = self.resolve(&run.segments) {
            self.named.push((resolved, run.line));
        }
    }

    /// Resolves a path through the imports: `io::stderr` with `use std::io`
    /// becomes `std::io::stderr`.
    fn resolve(&self, segments: &[String]) -> Option<String> {
        let (first, rest) = segments.split_first()?;
        let head = if first == "std" {
            "std".to_owned()
        } else {
            self.imports.get(first)?.clone()
        };
        Some(rest.iter().fold(head, |path, segment| join(&path, segment)))
    }
}

/// `prefix::segment`, or `segment` under an empty prefix.
fn join(prefix: &str, segment: &str) -> String {
    if prefix.is_empty() {
        segment.to_owned()
    } else {
        format!("{prefix}::{segment}")
    }
}

impl<'tree> Visit<'tree> for Paths {
    fn visit_item_use(&mut self, node: &'tree syn::ItemUse) {
        self.record_use_tree(&node.tree, "");
        visit::visit_item_use(self, node);
    }

    fn visit_macro(&mut self, node: &'tree syn::Macro) {
        self.record_macro_tokens(node.tokens.clone());
        visit::visit_macro(self, node);
    }

    fn visit_path(&mut self, node: &'tree syn::Path) {
        let segments: Vec<String> = node
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        if let Some(resolved) = self.resolve(&segments) {
            self.named.push((resolved, line_of(node)));
        }
        visit::visit_path(self, node);
    }
}

/// The blocking check.
///
/// # Errors
///
/// [`PolicyError`] when a file cannot be read or does not parse.
pub fn check(root: &Path) -> Result<Vec<Violation>, PolicyError> {
    let mut violations = Vec::new();
    let exception = root.join(EXCEPTION);
    for sources in ASYNCHRONOUS_SOURCES {
        for path in files_under(root, &root.join(sources))? {
            if path == exception || path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let file = parse_rust(&path)?;
            let mut paths = Paths::default();
            paths.visit_file(&file);
            for (named, line) in paths.named {
                if let Some(facility) = forbidden_facility(&named) {
                    violations.push(Violation {
                        path: relative(root, &path),
                        line: Some(line),
                        rule: RULE,
                        detail: format!("`{facility}` blocks; use the async runtime's equivalent"),
                    });
                }
            }
            for (module, line) in paths.globs {
                violations.push(Violation {
                    path: relative(root, &path),
                    line: Some(line),
                    rule: RULE,
                    detail: format!(
                        "a glob import of `{module}` hides which of its facilities are used; name them"
                    ),
                });
            }
        }
    }
    Ok(violations)
}
