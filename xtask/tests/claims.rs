//! The claims gate proven on synthetic roots and a captured report, so none of
//! it needs a container: the registry loads and rejects what it should, the
//! selection resolves the three selections and enforces the product-code rule,
//! and a report is read into the right verdict per claim.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use xtask::claims::registry::{self, Claim, Proof, RegistryError};
use xtask::claims::selection::{self, Selection, SelectionError};
use xtask::claims::verify::{self, Invocation, Status};

/// A throwaway directory tree, removed when it drops.
struct Tree {
    /// The tree's root.
    root: PathBuf,
}

impl Tree {
    /// A fresh, empty tree under the system's temporary directory.
    ///
    /// # Errors
    ///
    /// When the directory cannot be created.
    fn new(label: &str) -> Result<Tree, std::io::Error> {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let ordinal = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "iznik-claims-{}-{ordinal}-{label}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        Ok(Tree { root })
    }

    /// The tree's root.
    fn root(&self) -> &Path {
        &self.root
    }

    /// Writes a file under the root, creating its parent directories.
    ///
    /// # Errors
    ///
    /// When the file or its parents cannot be written.
    fn write(&self, relative: &str, contents: &str) -> Result<(), std::io::Error> {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)
    }

    /// Declares a task by writing a task file whose frontmatter names its id.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    fn task(&self, id: &str) -> Result<(), std::io::Error> {
        self.write(
            &format!("docs/plans/plan/tasks/{id}.md"),
            &format!("---\nid: {id}\n---\n"),
        )
    }

    /// Writes a task's claims file.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    fn claims(&self, task: &str, contents: &str) -> Result<(), std::io::Error> {
        self.write(&format!("regression/claims/{task}.toml"), contents)
    }

    /// Writes a scenario under a task.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    fn scenario(&self, task: &str, name: &str, contents: &str) -> Result<(), std::io::Error> {
        self.write(
            &format!("regression/scenarios/{task}/{name}.toml"),
            contents,
        )
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.root);
    }
}

/// A claims file declaring one claim proven by a scenario named `one`.
const ALPHA_CLAIMS: &str = r#"
[[claim]]
id = "alpha-one"
statement = "Alpha does its one thing."
scenario = "one"
"#;

/// The scenario `one`, naming the claim it proves.
const ONE_SCENARIO: &str = r#"
name = "one"
claims = ["alpha-one"]
driver = "engine"
budget_seconds = 30

[setup]
hosts = 1
files = []

[[steps]]
id = "s"
container = "engine"
run = "true"
timeout_seconds = 10
"#;

/// A well-formed registry loads, and its one claim is the scenario proof.
///
/// # Panics
///
/// When it does not load, or the claim is not the one declared.
#[test]
fn claims_a_well_formed_registry_loads() {
    let tree = Tree::new("valid").expect("a tree");
    tree.task("alpha").expect("the task");
    tree.claims("alpha", ALPHA_CLAIMS).expect("the claims");
    tree.scenario("alpha", "one", ONE_SCENARIO)
        .expect("the scenario");
    let registry = registry::load(tree.root()).expect("it loads");
    assert_eq!(registry.claims().len(), 1);
    let claim = registry.claims().first().expect("a claim");
    assert_eq!(claim.id, "alpha-one");
    assert_eq!(
        claim.proof,
        Proof::Scenario {
            name: "one".to_owned()
        }
    );
}

/// Two claims that share an id are rejected, the message naming both files.
///
/// # Panics
///
/// When it is accepted, or the message does not name both files.
#[test]
fn claims_a_duplicate_id_names_both_files() {
    let tree = Tree::new("duplicate").expect("a tree");
    tree.task("alpha").expect("alpha");
    tree.task("beta").expect("beta");
    tree.claims("alpha", ALPHA_CLAIMS).expect("alpha claims");
    tree.scenario("alpha", "one", ONE_SCENARIO)
        .expect("alpha scenario");
    tree.claims(
        "beta",
        "[[claim]]\nid = \"alpha-one\"\nstatement = \"Beta clashes.\"\ntest = \"p::b::t\"\nbecause = \"a unit proves it\"\n",
    )
    .expect("beta claims");
    let error = registry::load(tree.root()).expect_err("a clash is rejected");
    assert!(
        matches!(error, RegistryError::DuplicateId { .. }),
        "{error}"
    );
    let message = error.to_string();
    assert!(
        message.contains("alpha.toml") && message.contains("beta.toml"),
        "{message}"
    );
}

/// A test proof without a `because` is rejected.
///
/// # Panics
///
/// When it is accepted.
#[test]
fn claims_a_test_proof_without_because_is_rejected() {
    let tree = Tree::new("because").expect("a tree");
    tree.task("alpha").expect("the task");
    tree.claims(
        "alpha",
        "[[claim]]\nid = \"alpha-one\"\nstatement = \"No reason given.\"\ntest = \"p::b::t\"\n",
    )
    .expect("the claims");
    let error = registry::load(tree.root()).expect_err("rejected");
    assert!(
        matches!(error, RegistryError::TestWithoutBecause { .. }),
        "{error}"
    );
}

/// A claim naming both a scenario and a test is rejected, and so is one naming
/// neither.
///
/// # Panics
///
/// When either is accepted.
#[test]
fn claims_a_claim_names_one_proof() {
    let tree = Tree::new("both").expect("a tree");
    tree.task("alpha").expect("the task");
    tree.claims(
        "alpha",
        "[[claim]]\nid = \"alpha-one\"\nstatement = \"Two proofs.\"\nscenario = \"one\"\ntest = \"p::b::t\"\nbecause = \"why\"\n",
    )
    .expect("both");
    let both = registry::load(tree.root()).expect_err("both rejected");
    assert!(matches!(both, RegistryError::BothProofs { .. }), "{both}");

    tree.claims(
        "alpha",
        "[[claim]]\nid = \"alpha-one\"\nstatement = \"No proof.\"\n",
    )
    .expect("neither");
    let neither = registry::load(tree.root()).expect_err("neither rejected");
    assert!(
        matches!(neither, RegistryError::NeitherProof { .. }),
        "{neither}"
    );
}

/// A claims file named for no task is rejected.
///
/// # Panics
///
/// When it is accepted.
#[test]
fn claims_a_file_for_no_task_is_rejected() {
    let tree = Tree::new("no-task").expect("a tree");
    tree.claims("ghost", ALPHA_CLAIMS).expect("the claims");
    let error = registry::load(tree.root()).expect_err("rejected");
    assert!(
        matches!(error, RegistryError::UnknownTask { .. }),
        "{error}"
    );
}

/// A scenario proof whose scenario file does not exist is rejected.
///
/// # Panics
///
/// When it is accepted.
#[test]
fn claims_a_scenario_proof_without_a_file_is_rejected() {
    let tree = Tree::new("no-scenario").expect("a tree");
    tree.task("alpha").expect("the task");
    tree.claims("alpha", ALPHA_CLAIMS).expect("the claims");
    let error = registry::load(tree.root()).expect_err("rejected");
    assert!(
        matches!(error, RegistryError::ScenarioFileMissing { .. }),
        "{error}"
    );
}

/// A scenario of a registered task naming a claim declared nowhere is rejected.
///
/// # Panics
///
/// When it is accepted.
#[test]
fn claims_a_scenario_naming_an_undeclared_claim_is_rejected() {
    let tree = Tree::new("undeclared").expect("a tree");
    tree.task("alpha").expect("the task");
    tree.claims("alpha", ALPHA_CLAIMS).expect("the claims");
    tree.scenario(
        "alpha",
        "one",
        "name = \"one\"\nclaims = [\"alpha-one\", \"alpha-ghost\"]\ndriver = \"engine\"\nbudget_seconds = 30\n\n[setup]\nhosts = 1\nfiles = []\n\n[[steps]]\nid = \"s\"\ncontainer = \"engine\"\nrun = \"true\"\ntimeout_seconds = 10\n",
    )
    .expect("the scenario");
    let error = registry::load(tree.root()).expect_err("rejected");
    assert!(
        matches!(error, RegistryError::ScenarioClaimUndeclared { .. }),
        "{error}"
    );
}

/// Runs a `git` command in a tree, returning its standard output.
///
/// # Errors
///
/// When git cannot be run or exits non-zero.
fn git(tree: &Tree, arguments: &[&str]) -> Result<String, std::io::Error> {
    let output = Command::new("git")
        .current_dir(tree.root())
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Initialises a repository whose one branch is `develop`, with everything then
/// present committed as the baseline.
///
/// # Errors
///
/// When any git command fails.
fn develop_baseline(tree: &Tree) -> Result<(), std::io::Error> {
    git(tree, &["init", "-q"])?;
    git(tree, &["config", "user.email", "test@example.com"])?;
    git(tree, &["config", "user.name", "Test"])?;
    git(tree, &["add", "-A"])?;
    git(tree, &["commit", "-q", "-m", "baseline", "--allow-empty"])?;
    git(tree, &["branch", "-M", "develop"])?;
    Ok(())
}

/// On a branch whose diff since the merge base changes two claims files, both
/// tasks are selected.
///
/// # Panics
///
/// When the two tasks are not the selection.
#[test]
fn claims_two_changed_files_select_both_tasks() {
    let tree = Tree::new("two").expect("a tree");
    tree.task("alpha").expect("alpha");
    tree.task("beta").expect("beta");
    tree.claims("alpha", "# placeholder\n")
        .expect("alpha placeholder");
    develop_baseline(&tree).expect("the baseline");
    git(&tree, &["checkout", "-q", "-b", "feature"]).expect("the branch");
    tree.claims("alpha", ALPHA_CLAIMS).expect("alpha changed");
    tree.claims("beta", "# beta\n").expect("beta added");
    git(&tree, &["add", "-A"]).expect("staged");
    git(&tree, &["commit", "-q", "-m", "declare"]).expect("committed");
    let selected = selection::select(tree.root(), &Selection::CurrentBranch).expect("selected");
    assert_eq!(selected, vec!["alpha".to_owned(), "beta".to_owned()]);
}

/// A branch that changes product code while changing no claims file fails,
/// naming the rule — but only while a registry exists.
///
/// # Panics
///
/// When it does not fail, or fails for the wrong reason.
#[test]
fn claims_product_code_without_claims_fails() {
    let tree = Tree::new("teeth").expect("a tree");
    tree.task("alpha").expect("alpha");
    tree.claims("alpha", "# placeholder\n")
        .expect("a registry exists");
    develop_baseline(&tree).expect("the baseline");
    git(&tree, &["checkout", "-q", "-b", "feature"]).expect("the branch");
    tree.write("crates/iznik-server/src/thing.rs", "// changed\n")
        .expect("product code");
    git(&tree, &["add", "-A"]).expect("staged");
    git(&tree, &["commit", "-q", "-m", "code"]).expect("committed");
    let error =
        selection::select(tree.root(), &Selection::CurrentBranch).expect_err("the rule bites");
    assert!(
        matches!(error, SelectionError::ProductCodeWithoutClaims { .. }),
        "{error}"
    );
}

/// A branch that changes only a test file selects nothing and passes.
///
/// # Panics
///
/// When it does not select the empty set.
#[test]
fn claims_a_test_change_selects_nothing() {
    let tree = Tree::new("tests-only").expect("a tree");
    tree.task("alpha").expect("alpha");
    tree.claims("alpha", "# placeholder\n")
        .expect("a registry exists");
    develop_baseline(&tree).expect("the baseline");
    git(&tree, &["checkout", "-q", "-b", "feature"]).expect("the branch");
    tree.write("crates/iznik-server/tests/thing.rs", "// test\n")
        .expect("a test file");
    git(&tree, &["add", "-A"]).expect("staged");
    git(&tree, &["commit", "-q", "-m", "test"]).expect("committed");
    let selected = selection::select(tree.root(), &Selection::CurrentBranch).expect("selected");
    assert!(selected.is_empty(), "{selected:?}");
}

/// With no registry, a branch that changes product code still selects nothing:
/// the rule is inactive until the registry exists.
///
/// # Panics
///
/// When the rule bites without a registry.
#[test]
fn claims_no_registry_leaves_the_rule_off() {
    let tree = Tree::new("no-registry").expect("a tree");
    tree.task("alpha").expect("alpha");
    develop_baseline(&tree).expect("the baseline");
    git(&tree, &["checkout", "-q", "-b", "feature"]).expect("the branch");
    tree.write("crates/iznik-server/src/thing.rs", "// changed\n")
        .expect("product code");
    git(&tree, &["add", "-A"]).expect("staged");
    git(&tree, &["commit", "-q", "-m", "code"]).expect("committed");
    let selected = selection::select(tree.root(), &Selection::CurrentBranch).expect("selected");
    assert!(selected.is_empty(), "{selected:?}");
}

/// Explicit tasks are the selection, sorted and unique.
///
/// # Panics
///
/// When they are not.
#[test]
fn claims_explicit_tasks_are_the_selection() {
    let tree = Tree::new("explicit").expect("a tree");
    let selection = Selection::Tasks(vec![
        "beta".to_owned(),
        "alpha".to_owned(),
        "beta".to_owned(),
    ]);
    let selected = selection::select(tree.root(), &selection).expect("selected");
    assert_eq!(selected, vec!["alpha".to_owned(), "beta".to_owned()]);
}

/// A claim proven by a scenario, task and id.
fn scenario_claim(task: &str, id: &str, name: &str) -> Claim {
    Claim {
        task: task.to_owned(),
        id: id.to_owned(),
        statement: format!("{id} holds"),
        proof: Proof::Scenario {
            name: name.to_owned(),
        },
        platform: None,
        profile: None,
    }
}

/// A captured report is read into a verdict per claim: proven, failed, missing,
/// and — for a foreign platform — deferred.
///
/// # Panics
///
/// When any verdict is wrong.
#[test]
fn claims_a_captured_report_becomes_verdicts() {
    let report = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claims/report.xml"),
    )
    .expect("the fixture");
    let proven = scenario_claim("demo", "demo-passes", "passes");
    let failed = Claim {
        proof: Proof::Test {
            name: "demo::unit::demo_fails".to_owned(),
            because: "a unit proves it".to_owned(),
        },
        ..scenario_claim("demo", "demo-fails", "unused")
    };
    let missing = scenario_claim("demo", "demo-absent", "absent");
    let deferred = Claim {
        platform: Some("plan9".to_owned()),
        ..scenario_claim("demo", "demo-elsewhere", "passes")
    };
    let claims = [&proven, &failed, &missing, &deferred];
    let outcomes = verify::interpret_report(&claims, &report);
    assert_eq!(outcomes[0].status, Status::Proven);
    assert!(
        matches!(outcomes[1].status, Status::Failed { .. }),
        "{:?}",
        outcomes[1].status
    );
    assert_eq!(outcomes[2].status, Status::Missing);
    assert!(
        matches!(outcomes[3].status, Status::Deferred { .. }),
        "{:?}",
        outcomes[3].status
    );
    if let Status::Failed { detail } = &outcomes[1].status {
        assert!(detail.contains("demo_fails"), "{detail}");
    }
}

/// The plan unions a scenario proof and a test proof into one invocation, whose
/// filterset carries both atoms and whose packages are exactly the two named.
///
/// # Panics
///
/// When the filterset or packages are wrong.
#[test]
fn claims_the_filter_unions_the_proofs() {
    let scenario = scenario_claim("demo", "demo-one", "one");
    let test = Claim {
        proof: Proof::Test {
            name: "iznik-harness::report::report_round_trips".to_owned(),
            because: "no container is needed".to_owned(),
        },
        ..scenario_claim("demo", "demo-two", "unused")
    };
    let claims = [&scenario, &test];
    let plan = verify::plan(&claims);
    assert_eq!(plan.len(), 1);
    let invocation = plan.first().expect("one invocation");
    assert!(
        invocation.filter.contains("test(=scenario::demo::one)"),
        "{}",
        invocation.filter
    );
    assert!(
        invocation
            .filter
            .contains("package(iznik-harness) & binary(report) & test(=report_round_trips)"),
        "{}",
        invocation.filter
    );
    assert!(invocation.filter.contains(" | "), "{}", invocation.filter);
    assert_eq!(
        invocation.packages,
        vec!["iznik-harness".to_owned(), "iznik-regression".to_owned()]
    );
}

/// A `profile = "regression"` proof runs in its own invocation, whose command
/// line carries `--cargo-profile regression`.
///
/// # Panics
///
/// When it is not a separate invocation, or the flag is absent.
#[test]
fn claims_a_regression_profile_runs_in_its_own_invocation() {
    let default = scenario_claim("demo", "demo-default", "one");
    let regression = Claim {
        profile: Some("regression".to_owned()),
        ..scenario_claim("demo", "demo-regression", "two")
    };
    let claims = [&default, &regression];
    let plan = verify::plan(&claims);
    assert_eq!(plan.len(), 2);
    let under_regression: Vec<&Invocation> = plan
        .iter()
        .filter(|invocation| invocation.profile.as_deref() == Some("regression"))
        .collect();
    assert_eq!(under_regression.len(), 1);
    let line = verify::command_line(under_regression.first().expect("one"));
    let window = line
        .windows(2)
        .any(|pair| pair == ["--cargo-profile", "regression"]);
    assert!(window, "{line:?}");
    let default_line = verify::command_line(
        plan.iter()
            .find(|invocation| invocation.profile.is_none())
            .expect("a default invocation"),
    );
    assert!(
        !default_line
            .iter()
            .any(|argument| argument == "--cargo-profile"),
        "{default_line:?}"
    );
}

/// # Panics
///
/// When the platform a claims file names is not resolved to the machine it
/// means.
///
/// `darwin` is what plan 0004 writes and `macos` is what Rust calls the same
/// machine; a registry that took them for two platforms would defer the Darwin
/// artifacts' proof on a Mac as well, which is a proof that never runs.
#[test]
fn claims_a_platform_is_known_by_either_of_its_names() {
    assert!(
        verify::is_this_platform(std::env::consts::OS),
        "the machine running is the platform it says it is"
    );
    assert!(
        !verify::is_this_platform("plan9"),
        "and a platform nothing runs is not it"
    );
    assert_eq!(
        verify::is_this_platform("darwin"),
        verify::is_this_platform("macos"),
        "darwin and macos are one machine, whichever this is"
    );
}
