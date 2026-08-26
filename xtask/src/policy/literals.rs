//! No integer or float literal but `0` and `1` in expression position, outside
//! a constant's or static's initializer and an enum discriminant, in the
//! files that are not tests: every other value is a named constant whose
//! documentation says what the number is and why. An array-repeat count and
//! an array-type length are expressions for this purpose, and so is a literal
//! handed to a macro. Two readings beyond the letter: a literal in a pattern
//! (`2 => …`) is reported, since a constant matches as well as a literal
//! does; and a constant's type is visited while its initializer is not, so
//! `const BYTES: [u8; 4]` names its length in a constant of its own.

use std::path::Path;

use proc_macro2::TokenTree;
use syn::visit::{self, Visit};

use super::{PolicyError, Violation, is_fixture, line_of, parse_rust, relative, rust_files};

/// The rule a literal breaks.
const RULE: &str = "literal";

/// The directory names under which a file is a test and outside this rule.
const TEST_DIRECTORIES: &[&str] = &["tests", "benches"];

/// Whether a file is a test, a benchmark or a fixture, and so outside the
/// rule; the directories are judged on the path relative to the root, so a
/// root that itself lies under a `tests/` directory is not exempt wholesale.
fn is_exempt(root: &Path, path: &Path) -> bool {
    is_fixture(root, path)
        || relative(root, path).components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| TEST_DIRECTORIES.contains(&name))
        })
}

/// Whether a literal's value is one of the two a bare expression may hold.
fn is_zero_or_one(literal: &syn::Lit) -> bool {
    match literal {
        syn::Lit::Int(integer) => matches!(integer.base10_digits(), "0" | "1"),
        syn::Lit::Float(float) => matches!(
            float
                .base10_digits()
                .trim_end_matches('0')
                .trim_end_matches('.'),
            "0" | "1"
        ),
        _ => true,
    }
}

/// Collects the literals in expression position that are not `0` or `1`.
#[derive(Debug, Default)]
struct Literals {
    /// The offending literals as written, with their lines.
    found: Vec<(String, usize)>,
}

impl Literals {
    /// Judges one literal.
    fn consider(&mut self, literal: &syn::Lit) {
        if !is_zero_or_one(literal) {
            let text = match literal {
                syn::Lit::Int(integer) => integer.to_string(),
                syn::Lit::Float(float) => float.to_string(),
                _ => return,
            };
            self.found.push((text, line_of(literal)));
        }
    }

    /// Judges every literal token handed to a macro, groups included.
    fn consider_tokens(&mut self, tokens: proc_macro2::TokenStream) {
        for token in tokens {
            match token {
                TokenTree::Literal(literal) => self.consider(&syn::Lit::new(literal)),
                TokenTree::Group(group) => self.consider_tokens(group.stream()),
                TokenTree::Ident(_) | TokenTree::Punct(_) => {}
            }
        }
    }
}

impl<'tree> Visit<'tree> for Literals {
    fn visit_expr_lit(&mut self, node: &'tree syn::ExprLit) {
        self.consider(&node.lit);
        visit::visit_expr_lit(self, node);
    }

    fn visit_macro(&mut self, node: &'tree syn::Macro) {
        self.consider_tokens(node.tokens.clone());
        visit::visit_macro(self, node);
    }

    /// A constant's initializer is where literals live: not visited.
    fn visit_item_const(&mut self, node: &'tree syn::ItemConst) {
        self.visit_type(&node.ty);
    }

    /// A static's initializer is where literals live: not visited.
    fn visit_item_static(&mut self, node: &'tree syn::ItemStatic) {
        self.visit_type(&node.ty);
    }

    /// An associated constant's initializer is where literals live: not
    /// visited.
    fn visit_impl_item_const(&mut self, node: &'tree syn::ImplItemConst) {
        self.visit_type(&node.ty);
    }

    /// A trait's default constant is where literals live: not visited.
    fn visit_trait_item_const(&mut self, node: &'tree syn::TraitItemConst) {
        self.visit_type(&node.ty);
    }

    /// An enum discriminant is where literals live: the variant's fields are
    /// visited, its discriminant is not.
    fn visit_variant(&mut self, node: &'tree syn::Variant) {
        self.visit_fields(&node.fields);
    }
}

/// The literals check.
///
/// # Errors
///
/// [`PolicyError`] when a file cannot be read or does not parse.
pub fn check(root: &Path) -> Result<Vec<Violation>, PolicyError> {
    let mut violations = Vec::new();
    for path in rust_files(root)? {
        if is_exempt(root, &path) {
            continue;
        }
        let file = parse_rust(&path)?;
        let mut literals = Literals::default();
        literals.visit_file(&file);
        for (text, line) in literals.found {
            violations.push(Violation {
                path: relative(root, &path),
                line: Some(line),
                rule: RULE,
                detail: format!("`{text}` is a magic number; name it in a documented constant"),
            });
        }
    }
    Ok(violations)
}
