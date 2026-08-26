//! Every word of every declared identifier and of every file name under the
//! checked directories appears in the committed vocabulary, and every
//! vocabulary file is well formed and owned by a task.
//!
//! An identifier is split into words on `_`, on `-` and on case boundaries; a
//! run of digits belongs to the word before it (`utf8`, `sha256`) and a token
//! that is only digits is ignored (`x86_64` yields `x86`); leading and
//! trailing underscores and the bare `_` are ignored; a raw identifier loses
//! its `r#`. A declaration is what the file itself names: items, fields,
//! variants, function and closure parameters, local bindings, generic
//! parameters, lifetimes, labels and `use … as` renames. A used identifier
//! from another crate is that crate's to name.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};

use super::{PolicyError, Violation, files_under, line_of, parse_rust, read, relative, rust_files};

/// The directory of the vocabulary files, relative to the root.
const LEXICON_DIRECTORY: &str = "policy/lexicon";

/// The directory the plans live in, relative to the root; a vocabulary file
/// is named after a task under it.
const PLANS_DIRECTORY: &str = "docs/plans";

/// The directories whose file and directory names are held to the
/// vocabulary, relative to the root.
const NAMED_ROOTS: &[&str] = &["crates", "xtask", "regression", "policy"];

/// The rule an identifier breaks.
const IDENTIFIER_RULE: &str = "lexicon";

/// The rule a file or directory name breaks.
const NAME_RULE: &str = "lexicon-name";

/// The rule a vocabulary file breaks.
const FILE_RULE: &str = "lexicon-file";

/// The words of an identifier, lowercase, in order.
#[must_use]
pub fn words(identifier: &str) -> Vec<String> {
    let bare = identifier
        .strip_prefix("r#")
        .unwrap_or(identifier)
        .trim_matches('_');
    bare.split(['_', '-', '.'])
        .flat_map(split_on_case)
        .filter(|word| !word.chars().all(|character| character.is_ascii_digit()))
        .map(|word| word.to_lowercase())
        .collect()
}

/// Splits one underscore-free token on case boundaries: before an uppercase
/// letter that follows a lowercase letter or a digit, and before the last
/// uppercase letter of a run that is followed by a lowercase letter.
fn split_on_case(token: &str) -> Vec<String> {
    let characters: Vec<char> = token.chars().collect();
    let mut pieces = Vec::new();
    let mut current = String::new();
    for (index, character) in characters.iter().enumerate() {
        let previous = index
            .checked_sub(1)
            .and_then(|before| characters.get(before));
        let next = characters.get(index.saturating_add(1));
        let after_lower_or_digit =
            previous.is_some_and(|before| before.is_lowercase() || before.is_ascii_digit());
        let ends_an_upper_run = previous.is_some_and(char::is_ascii_uppercase)
            && next.is_some_and(char::is_ascii_lowercase);
        if character.is_uppercase()
            && (after_lower_or_digit || ends_an_upper_run)
            && !current.is_empty()
        {
            pieces.push(std::mem::take(&mut current));
        }
        current.push(*character);
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

/// A vocabulary file's line is one lowercase word.
fn is_well_formed_word(word: &str) -> bool {
    let mut characters = word.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && characters.all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
}

/// The task ids under the plans: the file names under `tasks/` without their
/// four-digit prefix and extension.
///
/// # Errors
///
/// [`PolicyError::Io`] when the plans cannot be listed.
fn task_ids(root: &Path) -> Result<BTreeSet<String>, PolicyError> {
    let mut ids = BTreeSet::new();
    for path in files_under(root, &root.join(PLANS_DIRECTORY))? {
        let is_task = path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|directory| directory == "tasks");
        if !is_task {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if let Some((_number, id)) = stem.split_once('-') {
            ids.insert(id.to_owned());
        }
    }
    Ok(ids)
}

/// Reads one vocabulary file, reporting what is wrong with it, and adds its
/// words to the vocabulary.
///
/// # Errors
///
/// [`PolicyError::Io`] when the file cannot be read.
fn load_lexicon_file(
    root: &Path,
    path: &Path,
    tasks: &BTreeSet<String>,
    vocabulary: &mut BTreeSet<String>,
    violations: &mut Vec<Violation>,
) -> Result<(), PolicyError> {
    let reported = relative(root, path);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    if !tasks.contains(stem) {
        violations.push(Violation {
            path: reported.clone(),
            line: None,
            rule: FILE_RULE,
            detail: format!("`{stem}` is not the id of a task under {PLANS_DIRECTORY}"),
        });
    }
    let mut previous: Option<&str> = None;
    let text = read(path)?;
    for (index, word) in text.lines().enumerate() {
        let line = Some(index.saturating_add(1));
        if !is_well_formed_word(word) {
            violations.push(Violation {
                path: reported.clone(),
                line,
                rule: FILE_RULE,
                detail: format!("`{word}` is not a lowercase word of letters and digits"),
            });
        }
        match previous {
            Some(before) if before == word => violations.push(Violation {
                path: reported.clone(),
                line,
                rule: FILE_RULE,
                detail: format!("`{word}` is listed twice"),
            }),
            Some(before) if before > word => violations.push(Violation {
                path: reported.clone(),
                line,
                rule: FILE_RULE,
                detail: format!("`{word}` is out of order after `{before}`"),
            }),
            _ => {}
        }
        previous = Some(word);
        vocabulary.insert(word.to_owned());
    }
    Ok(())
}

/// The vocabulary: the union of every file under `policy/lexicon/`, with the
/// violations of the files themselves.
///
/// # Errors
///
/// [`PolicyError::Io`] when the plans or a vocabulary file cannot be read.
fn vocabulary(
    root: &Path,
    violations: &mut Vec<Violation>,
) -> Result<BTreeSet<String>, PolicyError> {
    let tasks = task_ids(root)?;
    let mut vocabulary = BTreeSet::new();
    for path in files_under(root, &root.join(LEXICON_DIRECTORY))? {
        load_lexicon_file(root, &path, &tasks, &mut vocabulary, violations)?;
    }
    Ok(vocabulary)
}

/// A declared identifier and the line it is declared on.
#[derive(Debug)]
struct Declaration {
    /// The identifier as written.
    identifier: String,
    /// Its line, one-based.
    line: usize,
}

/// Collects every identifier a file declares.
#[derive(Debug, Default)]
struct Declarations {
    /// What was found, in source order.
    found: Vec<Declaration>,
}

impl Declarations {
    /// Records one identifier.
    fn declare(&mut self, identifier: &syn::Ident) {
        self.found.push(Declaration {
            identifier: identifier.to_string(),
            line: line_of(identifier),
        });
    }

    /// Records the generic parameters of an item.
    fn declare_generics(&mut self, generics: &syn::Generics) {
        for parameter in &generics.params {
            match parameter {
                syn::GenericParam::Lifetime(lifetime) => self.declare(&lifetime.lifetime.ident),
                syn::GenericParam::Type(parameter) => self.declare(&parameter.ident),
                syn::GenericParam::Const(parameter) => self.declare(&parameter.ident),
            }
        }
    }
}

impl<'tree> Visit<'tree> for Declarations {
    fn visit_item_fn(&mut self, node: &'tree syn::ItemFn) {
        self.declare(&node.sig.ident);
        self.declare_generics(&node.sig.generics);
        visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'tree syn::ImplItemFn) {
        self.declare(&node.sig.ident);
        self.declare_generics(&node.sig.generics);
        visit::visit_impl_item_fn(self, node);
    }

    fn visit_trait_item_fn(&mut self, node: &'tree syn::TraitItemFn) {
        self.declare(&node.sig.ident);
        self.declare_generics(&node.sig.generics);
        visit::visit_trait_item_fn(self, node);
    }

    fn visit_foreign_item_fn(&mut self, node: &'tree syn::ForeignItemFn) {
        self.declare(&node.sig.ident);
        visit::visit_foreign_item_fn(self, node);
    }

    fn visit_item_struct(&mut self, node: &'tree syn::ItemStruct) {
        self.declare(&node.ident);
        self.declare_generics(&node.generics);
        visit::visit_item_struct(self, node);
    }

    fn visit_item_enum(&mut self, node: &'tree syn::ItemEnum) {
        self.declare(&node.ident);
        self.declare_generics(&node.generics);
        visit::visit_item_enum(self, node);
    }

    fn visit_item_union(&mut self, node: &'tree syn::ItemUnion) {
        self.declare(&node.ident);
        self.declare_generics(&node.generics);
        visit::visit_item_union(self, node);
    }

    fn visit_item_type(&mut self, node: &'tree syn::ItemType) {
        self.declare(&node.ident);
        self.declare_generics(&node.generics);
        visit::visit_item_type(self, node);
    }

    fn visit_item_trait(&mut self, node: &'tree syn::ItemTrait) {
        self.declare(&node.ident);
        self.declare_generics(&node.generics);
        visit::visit_item_trait(self, node);
    }

    fn visit_item_impl(&mut self, node: &'tree syn::ItemImpl) {
        self.declare_generics(&node.generics);
        visit::visit_item_impl(self, node);
    }

    fn visit_item_const(&mut self, node: &'tree syn::ItemConst) {
        self.declare(&node.ident);
        visit::visit_item_const(self, node);
    }

    fn visit_item_static(&mut self, node: &'tree syn::ItemStatic) {
        self.declare(&node.ident);
        visit::visit_item_static(self, node);
    }

    fn visit_item_mod(&mut self, node: &'tree syn::ItemMod) {
        self.declare(&node.ident);
        visit::visit_item_mod(self, node);
    }

    fn visit_item_macro(&mut self, node: &'tree syn::ItemMacro) {
        if let Some(identifier) = &node.ident {
            self.declare(identifier);
        }
        visit::visit_item_macro(self, node);
    }

    fn visit_item_extern_crate(&mut self, node: &'tree syn::ItemExternCrate) {
        if let Some((_as, rename)) = &node.rename {
            self.declare(rename);
        }
        visit::visit_item_extern_crate(self, node);
    }

    fn visit_impl_item_const(&mut self, node: &'tree syn::ImplItemConst) {
        self.declare(&node.ident);
        visit::visit_impl_item_const(self, node);
    }

    fn visit_impl_item_type(&mut self, node: &'tree syn::ImplItemType) {
        self.declare(&node.ident);
        visit::visit_impl_item_type(self, node);
    }

    fn visit_trait_item_const(&mut self, node: &'tree syn::TraitItemConst) {
        self.declare(&node.ident);
        visit::visit_trait_item_const(self, node);
    }

    fn visit_trait_item_type(&mut self, node: &'tree syn::TraitItemType) {
        self.declare(&node.ident);
        visit::visit_trait_item_type(self, node);
    }

    fn visit_field(&mut self, node: &'tree syn::Field) {
        if let Some(identifier) = &node.ident {
            self.declare(identifier);
        }
        visit::visit_field(self, node);
    }

    fn visit_variant(&mut self, node: &'tree syn::Variant) {
        self.declare(&node.ident);
        visit::visit_variant(self, node);
    }

    fn visit_use_rename(&mut self, node: &'tree syn::UseRename) {
        self.declare(&node.rename);
        visit::visit_use_rename(self, node);
    }

    fn visit_label(&mut self, node: &'tree syn::Label) {
        self.declare(&node.name.ident);
        visit::visit_label(self, node);
    }

    /// A binding pattern declares its identifier — with one reading: a bare
    /// capitalized identifier in pattern position is a unit variant or a
    /// constant being matched, not a binding, and is not declared here.
    fn visit_pat_ident(&mut self, node: &'tree syn::PatIdent) {
        let capitalized = node
            .ident
            .to_string()
            .chars()
            .next()
            .is_some_and(char::is_uppercase);
        let plain = node.by_ref.is_none() && node.mutability.is_none() && node.subpat.is_none();
        if !(capitalized && plain) {
            self.declare(&node.ident);
        }
        visit::visit_pat_ident(self, node);
    }
}

/// Every word of every identifier a file declares, checked against the
/// vocabulary.
///
/// # Errors
///
/// [`PolicyError`] when the file cannot be read or does not parse.
fn check_identifiers(
    root: &Path,
    path: &Path,
    vocabulary: &BTreeSet<String>,
    violations: &mut Vec<Violation>,
) -> Result<(), PolicyError> {
    let file = parse_rust(path)?;
    let mut declarations = Declarations::default();
    declarations.visit_file(&file);
    for declaration in declarations.found {
        for word in words(&declaration.identifier) {
            if !vocabulary.contains(&word) {
                violations.push(Violation {
                    path: relative(root, path),
                    line: Some(declaration.line),
                    rule: IDENTIFIER_RULE,
                    detail: format!(
                        "`{}`: `{word}` is not in the vocabulary",
                        declaration.identifier
                    ),
                });
            }
        }
    }
    Ok(())
}

/// Every word of every file and directory name under the named roots,
/// checked against the vocabulary.
///
/// # Errors
///
/// [`PolicyError::Io`] when a directory cannot be listed.
fn check_names(
    root: &Path,
    vocabulary: &BTreeSet<String>,
    violations: &mut Vec<Violation>,
) -> Result<(), PolicyError> {
    let mut names: BTreeSet<PathBuf> = BTreeSet::new();
    for named_root in NAMED_ROOTS {
        let directory = root.join(named_root);
        for file in files_under(root, &directory)? {
            let mut ancestor = file.as_path();
            while let Some(parent) = ancestor.parent() {
                names.insert(ancestor.to_path_buf());
                if parent == root {
                    break;
                }
                ancestor = parent;
            }
        }
    }
    for path in names {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        for word in words(name) {
            if !vocabulary.contains(&word) {
                violations.push(Violation {
                    path: relative(root, &path),
                    line: None,
                    rule: NAME_RULE,
                    detail: format!("`{name}`: `{word}` is not in the vocabulary"),
                });
            }
        }
    }
    Ok(())
}

/// The vocabulary check.
///
/// # Errors
///
/// [`PolicyError`] when a file cannot be read or a source file does not
/// parse.
pub fn check(root: &Path) -> Result<Vec<Violation>, PolicyError> {
    let mut violations = Vec::new();
    let vocabulary = vocabulary(root, &mut violations)?;
    for path in rust_files(root)? {
        check_identifiers(root, &path, &vocabulary, &mut violations)?;
    }
    check_names(root, &vocabulary, &mut violations)?;
    Ok(violations)
}
