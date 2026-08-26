//! No `allow`, `expect` or `cfg(test)` attribute, inner or outer, anywhere
//! under `crates/` and `xtask/`, tests included — a `cfg_attr` that carries
//! either is the same thing behind a predicate. A lint that fires on correct
//! code is either a bug in the code's shape or a lint that is wrong for this
//! codebase, never a waiver at one site; and a `#[cfg(test)]` module would
//! see two copies of every type through the development-dependency cycle,
//! so every test is a file under `tests/`.

use std::path::Path;

use proc_macro2::TokenTree;
use syn::visit::{self, Visit};

use super::{PolicyError, Violation, line_of, parse_rust, relative, rust_files};

/// The attributes that waive a lint.
const WAIVERS: &[&str] = &["allow", "expect"];

/// The rule a waiver breaks.
const WAIVER_RULE: &str = "attribute-waiver";

/// The rule a `cfg(test)` breaks.
const CFG_TEST_RULE: &str = "attribute-cfg-test";

/// Whether a token stream mentions an identifier, at any depth.
fn mentions(tokens: proc_macro2::TokenStream, wanted: &str) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Ident(identifier) => identifier == wanted,
        TokenTree::Group(group) => mentions(group.stream(), wanted),
        TokenTree::Literal(_) | TokenTree::Punct(_) => false,
    })
}

/// Collects the offending attributes.
#[derive(Debug, Default)]
struct Attributes {
    /// The rule broken, what the attribute is, and its line.
    found: Vec<(&'static str, String, usize)>,
}

impl Attributes {
    /// Records one offending attribute.
    fn record(&mut self, rule: &'static str, attribute: String, node: &syn::Attribute) {
        self.found.push((rule, attribute, line_of(node)));
    }

    /// Judges a `cfg` or `cfg_attr` attribute's tokens: a `test` predicate,
    /// and, behind a `cfg_attr`, a waiver.
    fn judge_configuration(&mut self, node: &syn::Attribute, attribute: &str) {
        let syn::Meta::List(list) = &node.meta else {
            return;
        };
        if mentions(list.tokens.clone(), "test") {
            self.record(CFG_TEST_RULE, format!("#[{attribute}(test)]"), node);
        }
        if attribute != "cfg_attr" {
            return;
        }
        for waiver in WAIVERS {
            if mentions(list.tokens.clone(), waiver) {
                self.record(WAIVER_RULE, format!("#[{attribute}(..., {waiver})]"), node);
            }
        }
    }
}

impl<'tree> Visit<'tree> for Attributes {
    fn visit_attribute(&mut self, node: &'tree syn::Attribute) {
        let path = node.path();
        if let Some(waiver) = WAIVERS.iter().find(|waiver| path.is_ident(waiver)) {
            self.record(WAIVER_RULE, format!("#[{waiver}]"), node);
        } else if path.is_ident("cfg") {
            self.judge_configuration(node, "cfg");
        } else if path.is_ident("cfg_attr") {
            self.judge_configuration(node, "cfg_attr");
        }
        visit::visit_attribute(self, node);
    }
}

/// The attributes check.
///
/// # Errors
///
/// [`PolicyError`] when a file cannot be read or does not parse.
pub fn check(root: &Path) -> Result<Vec<Violation>, PolicyError> {
    let mut violations = Vec::new();
    for path in rust_files(root)? {
        let file = parse_rust(&path)?;
        let mut attributes = Attributes::default();
        attributes.visit_file(&file);
        for (rule, attribute, line) in attributes.found {
            let detail = if rule == WAIVER_RULE {
                format!(
                    "`{attribute}` waives a lint at one site; restructure the code or change the rule for everyone"
                )
            } else {
                format!("`{attribute}` makes a test module; every test is a file under `tests/`")
            };
            violations.push(Violation {
                path: relative(root, &path),
                line: Some(line),
                rule,
                detail,
            });
        }
    }
    Ok(violations)
}
