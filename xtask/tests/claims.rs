//! The claims gate proven on synthetic roots and a captured report, so none of
//! it needs a container: the registry loads and rejects what it should, the
//! selection resolves the three selections and enforces the product-code rule,
//! and a report is read into the right verdict per claim.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use xtask::claims::registry::{self, Claim, Proof, RegistryError};
use xtask::claims::render;
use xtask::claims::selection::{self, Selection, SelectionError};
use xtask::claims::verify::{self, Invocation, Outcome, Report, Status};

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
/// The developer's own Git configuration is left out of it: `core.hooksPath`
/// is what Git documents as the way to switch every hook off, and a hook a
/// developer has installed — a `commit-msg` that reads the subject of these
/// fixture commits, say — would otherwise decide whether these cases pass on
/// whose machine runs them. What is under test is the selection, not the
/// machine.
///
/// # Errors
///
/// When git cannot be run or exits non-zero.
fn git(tree: &Tree, arguments: &[&str]) -> Result<String, std::io::Error> {
    let output = Command::new("git")
        .current_dir(tree.root())
        .args(["-c", "core.hooksPath=/dev/null"])
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

/// On the trunk the merge base is `HEAD`, so the last commit is what is
/// compared: a commit to `develop` that changes product code and no claims
/// breaks the rule there too, rather than selecting nothing.
///
/// # Panics
///
/// When the rule does not bite on the trunk.
#[test]
fn claims_on_the_trunk_the_last_commit_is_checked() {
    let tree = Tree::new("trunk-code").expect("a tree");
    tree.task("alpha").expect("alpha");
    tree.claims("alpha", "# placeholder\n")
        .expect("a registry exists");
    develop_baseline(&tree).expect("the baseline");
    tree.write("crates/iznik-server/src/thing.rs", "// changed\n")
        .expect("product code");
    git(&tree, &["add", "-A"]).expect("staged");
    git(&tree, &["commit", "-q", "-m", "code on develop"]).expect("committed");
    let error = selection::select(tree.root(), &Selection::CurrentBranch)
        .expect_err("the rule bites on the trunk");
    assert!(
        matches!(error, SelectionError::ProductCodeWithoutClaims { .. }),
        "{error}"
    );
}

/// On the trunk, the claims the last commit declares are selected, and so
/// are claims in the working tree that are not committed yet, untracked
/// files included.
///
/// # Panics
///
/// When either is not selected.
#[test]
fn claims_on_the_trunk_the_last_commit_and_the_working_tree_select() {
    let tree = Tree::new("trunk-claims").expect("a tree");
    tree.task("alpha").expect("alpha");
    tree.task("beta").expect("beta");
    tree.claims("alpha", "# placeholder\n")
        .expect("a registry exists");
    develop_baseline(&tree).expect("the baseline");
    tree.claims("alpha", ALPHA_CLAIMS).expect("alpha changed");
    git(&tree, &["add", "-A"]).expect("staged");
    git(&tree, &["commit", "-q", "-m", "declare on develop"]).expect("committed");
    let selected = selection::select(tree.root(), &Selection::CurrentBranch).expect("selected");
    assert_eq!(selected, vec!["alpha".to_owned()]);

    tree.claims(
        "beta",
        "[[claim]]\nid = \"beta-one\"\nstatement = \"Beta.\"\nscenario = \"one\"\n",
    )
    .expect("beta, untracked");
    let with_untracked =
        selection::select(tree.root(), &Selection::CurrentBranch).expect("selected");
    assert_eq!(with_untracked, vec!["alpha".to_owned(), "beta".to_owned()]);
}

/// A claims file edited so that it parses to the same TOML — a comment,
/// spacing, the order of keys — declares nothing new: it selects nothing and
/// does not satisfy the product-code rule.
///
/// # Panics
///
/// When a cosmetic edit counts as declaring claims.
#[test]
fn claims_an_identical_claims_file_does_not_count() {
    let tree = Tree::new("cosmetic").expect("a tree");
    tree.task("alpha").expect("alpha");
    tree.claims("alpha", ALPHA_CLAIMS).expect("alpha");
    develop_baseline(&tree).expect("the baseline");
    git(&tree, &["checkout", "-q", "-b", "feature"]).expect("the branch");
    tree.claims(
        "alpha",
        "# Only the layout changed.\n[[claim]]\nscenario   = \"one\"\nid = \"alpha-one\"\nstatement = \"Alpha does its one thing.\"\n",
    )
    .expect("reformatted");
    git(&tree, &["add", "-A"]).expect("staged");
    git(&tree, &["commit", "-q", "-m", "a comment"]).expect("committed");
    let selected = selection::select(tree.root(), &Selection::CurrentBranch).expect("selected");
    assert!(selected.is_empty(), "{selected:?}");

    tree.write("crates/iznik-server/src/thing.rs", "// changed\n")
        .expect("product code");
    git(&tree, &["add", "-A"]).expect("staged");
    git(&tree, &["commit", "-q", "-m", "code and a comment"]).expect("committed");
    let error = selection::select(tree.root(), &Selection::CurrentBranch)
        .expect_err("a comment is not a claim");
    assert!(
        matches!(error, SelectionError::ProductCodeWithoutClaims { .. }),
        "{error}"
    );
}

/// A root whose workspace declares one package with one test target.
///
/// # Errors
///
/// When a file cannot be written.
fn workspace_with_a_test_target(tree: &Tree) -> Result<(), std::io::Error> {
    tree.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/demo\", \"crates/mimic\"]\n",
    )?;
    tree.write("crates/demo/Cargo.toml", "[package]\nname = \"demo\"\n")?;
    tree.write("crates/demo/tests/unit.rs", "")?;
    tree.write(
        "crates/mimic/Cargo.toml",
        "[package]\nname = \"mimic\"\n\n[[test]]\nname = \"scenarios\"\nharness = false\n",
    )
}

/// A test proof loads when its package and test target exist, as a file
/// under `tests/` or as a `[[test]]`, and is rejected, naming the proof, when
/// the package, the target or the test is missing.
///
/// # Panics
///
/// When a real target is rejected or a missing one is accepted.
#[test]
fn claims_a_test_proof_names_an_existing_test_target() {
    let tree = Tree::new("test-target").expect("a tree");
    tree.task("alpha").expect("the task");
    workspace_with_a_test_target(&tree).expect("the workspace");
    let claim = |test: &str| {
        format!(
            "[[claim]]\nid = \"alpha-one\"\nstatement = \"It holds.\"\ntest = \"{test}\"\nbecause = \"a unit proves it\"\n"
        )
    };
    for present in ["demo::unit::passes", "mimic::scenarios::a::b"] {
        tree.claims("alpha", &claim(present)).expect("the claims");
        registry::load(tree.root()).unwrap_or_else(|error| panic!("{present}: {error}"));
    }
    for missing in [
        "demo::absent::passes",
        "ghost::unit::passes",
        "mimic::other::passes",
        "demo::unit",
        "demo::unit::",
    ] {
        tree.claims("alpha", &claim(missing)).expect("the claims");
        let error = registry::load(tree.root()).expect_err(missing);
        assert!(
            matches!(error, RegistryError::TestTargetMissing { .. }),
            "{missing}: {error}"
        );
        assert!(error.to_string().contains(missing), "{error}");
    }
}

/// The repository's own registry loads, so every test proof it declares
/// names a test target that exists.
///
/// # Panics
///
/// When it does not load.
#[test]
fn claims_the_real_registry_loads() {
    registry::load(&xtask::repository_root()).expect("the registry loads");
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
        verify::resolves("darwin", "macos"),
        "`darwin` is what a claims file writes for the machine Rust calls `macos`"
    );
    assert!(
        !verify::resolves("macos", "darwin"),
        "and the alias goes one way: nothing reports itself as running `darwin`"
    );
    assert!(
        verify::resolves("linux", "linux"),
        "a platform that needs no alias is itself"
    );
    assert!(
        !verify::resolves("plan9", "linux"),
        "and a platform nothing runs is not one of them"
    );
    assert!(
        verify::is_this_platform(std::env::consts::OS),
        "and the machine running is the platform it says it is"
    );
}

/// A recorded display measurement stays deferred on the current operating system.
///
/// # Panics
/// Fails if a display record schedules a test or becomes proven from a report.
#[test]
fn claims_display_records_stay_deferred_on_the_running_platform() {
    let tree = Tree::new("display").expect("tree");
    tree.task("alpha").expect("task");
    tree.write(
        "docs/notes/display.md",
        "# Display measurement\nDeferred.\n",
    )
    .expect("record");
    tree.claims("alpha", &format!(
        "[[claim]]\nid = \"demo-passes\"\nstatement = \"Native timing is measured.\"\ndisplay = \"docs/notes/display.md\"\nbecause = \"requires a native display\"\nplatform = \"{}\"\n",
        std::env::consts::OS
    )).expect("claim");
    let registry = registry::load(tree.root()).expect("valid registry");
    let claims: Vec<_> = registry.claims().iter().collect();
    assert!(
        verify::plan(&claims).is_empty(),
        "display records never invoke nextest"
    );
    let captured = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claims/report.xml"),
    )
    .expect("report");
    for report in ["", captured.as_str()] {
        let outcomes = verify::interpret_report(&claims, report);
        assert_eq!(outcomes.len(), 1);
        let Status::Deferred { reason } = &outcomes[0].status else {
            panic!("display record must remain deferred: {:?}", outcomes[0]);
        };
        assert!(reason.contains("requires a native display"), "{reason}");
        assert!(reason.contains("docs/notes/display.md"), "{reason}");
        assert!(Report { outcomes }.holds());
    }
}

/// Display records obey the same exclusive category rule as automated proofs.
///
/// # Panics
/// Fails if a display record can be paired with another proof or omit its reason.
#[test]
fn claims_display_records_require_one_category_and_a_reason() {
    let tree = Tree::new("display-category").expect("tree");
    tree.task("alpha").expect("task");
    tree.write("docs/notes/display.md", "# Deferred\n")
        .expect("record");
    let header = "[[claim]]\nid = \"display\"\nstatement = \"Native timing.\"\ndisplay = \"docs/notes/display.md\"\n";
    for extra in [
        "scenario = \"one\"",
        "test = \"demo::unit::passes\"",
        "scenario = \"one\"\ntest = \"demo::unit::passes\"",
    ] {
        tree.claims(
            "alpha",
            &format!("{header}because = \"native display\"\n{extra}\n"),
        )
        .expect("claims");
        let error = registry::load(tree.root()).expect_err("ambiguous proof");
        assert!(matches!(error, RegistryError::BothProofs { .. }), "{error}");
    }
    for reason in ["", "because = \"\"", "because = \"   \""] {
        tree.claims("alpha", &format!("{header}{reason}\n"))
            .expect("claims");
        let error = registry::load(tree.root()).expect_err("missing reason");
        assert!(
            matches!(error, RegistryError::DisplayRecord { .. }),
            "{error}"
        );
    }
}

/// Records must be existing Markdown notes with normal repository-relative paths.
///
/// # Panics
/// Fails if a missing, external, traversing, directory or non-Markdown record loads.
#[test]
fn claims_display_records_require_an_existing_markdown_note() {
    let tree = Tree::new("display-path").expect("tree");
    tree.task("alpha").expect("task");
    tree.write("docs/notes/display.md", "# Deferred\n")
        .expect("record");
    tree.write("docs/display.md", "# Outside notes\n")
        .expect("outside");
    tree.write("docs/notes/display.txt", "Deferred\n")
        .expect("text");
    let absolute = tree
        .root()
        .join("docs/notes/display.md")
        .display()
        .to_string();
    for record in [
        "docs/notes/missing.md",
        "docs/display.md",
        "docs/notes/../display.md",
        "docs/notes/display.txt",
        "docs/notes",
        absolute.as_str(),
    ] {
        tree.claims("alpha", &format!("[[claim]]\nid = \"display\"\nstatement = \"Native timing.\"\ndisplay = {record:?}\nbecause = \"native display\"\n")).expect("claims");
        let error = registry::load(tree.root()).expect_err("invalid note");
        assert!(
            matches!(error, RegistryError::DisplayRecord { .. }),
            "{record}: {error}"
        );
    }
}

/// A deferred claim is printed apart from the proven ones, under a heading
/// that says it was neither proven nor failed, and the summary says the same.
///
/// # Panics
///
/// When a deferred claim is listed among the others or counted as proven.
#[test]
fn claims_deferred_claims_are_printed_apart() {
    let outcome = |id: &str, status: Status| Outcome {
        task: "alpha".to_owned(),
        id: id.to_owned(),
        statement: format!("{id} holds."),
        status,
    };
    let report = Report {
        outcomes: vec![
            outcome(
                "unmeasured",
                Status::Deferred {
                    reason: "unmeasured: a manual display measurement".to_owned(),
                },
            ),
            outcome("measured", Status::Proven),
        ],
    };
    let text = render(&report);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        vec![
            "proven: measured \u{2014} measured holds. (alpha)",
            "deferred \u{2014} not run here, so neither proven nor failed (1):",
            "deferred: unmeasured \u{2014} unmeasured holds. (alpha)",
            "    unmeasured: a manual display measurement",
            "claims: 1 proven, 0 failed, 0 missing, 1 deferred and not proven",
        ],
        "{text}"
    );
    assert!(report.holds(), "a deferred claim does not fail the gate");
}
