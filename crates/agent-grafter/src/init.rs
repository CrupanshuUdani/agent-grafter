//! `init`: generate a starter `agent-grafter.toml` -- interactively (a short
//! prompt flow), or non-interactively via `--print` (a commented template to
//! stdout, touching nothing).
//!
//! Implements Task S10; the open choices the brief left are resolved by
//! Rulings R1-R10 in `progress.md`, referenced by number in the doc comments
//! below. The short version:
//!
//!   - **R1**: no `--print` and stdin is not a terminal -> refuse (exit 2),
//!     naming `init --print > agent-grafter.toml` as the scripted route.
//!   - **R2**: `--print` is non-interactive, writes nothing, never prompts.
//!   - **R3**: no filesystem probing -- prompt defaults are static text, not
//!     detected facts.
//!   - **R4**: the existing-manifest check runs before any prompt, and the
//!     write itself uses `create_new(true)` (no check-then-write race).
//!   - **R5**: the generated TOML is run through `manifest::parse` before it
//!     is written *or* printed; a failure writes nothing.
//!   - **R6**: the final "write this?" confirm defaults to Yes when creating
//!     a new file, No when `--force` would overwrite one.
//!   - **R7**: cancellation (Ctrl-C/Esc, or an explicit "no" to the final
//!     confirm) writes nothing and is reported the same way.
//!   - **R8**: the question set -- source, `copy`, `link`, then one or more
//!     targets -- lives in [`run_prompts`].
//!   - **R9**: `manifest::ManifestError`'s `NotFound` hint pointing back to
//!     `init` lives in `manifest.rs`, not here.
//!   - **R10**: `cliclack`'s actual transitive-crate delta and gate results
//!     are recorded in this task's report, not in code.
//!
//! **Testability, mirroring `safelink::capability::SymlinkProbe` and
//! `crate::winenv::WindowsFacts`:** neither a closed nor a piped stdin
//! reaches `cliclack` at all -- both come back a hard I/O error, off a real
//! terminal, before a single prompt renders (see progress.md's corrected
//! "ground 4"). So every prompt is issued through the [`Prompter`] trait:
//! [`CliclackPrompter`] is the real, untestable-by-construction terminal
//! implementation, and [`ScriptedPrompter`] is what tests drive instead.
//! Only the thin call from `cli.rs` into [`run`] with a real [`CliclackPrompter`]
//! is untested -- a stated, acceptable gap, not an oversight.

use std::collections::VecDeque;
use std::io::{self, ErrorKind, Write as _};
use std::path::PathBuf;

use crate::manifest;

/// Arguments `init` needs, independent of how `clap` parsed them.
pub struct InitArgs {
    pub manifest_path: PathBuf,
    pub force: bool,
    pub print: bool,
}

/// Every way `init` can end. `cli.rs` maps each variant to an [`super::cli::ExitCode`]
/// and to the stdout/stderr text the user sees -- this module stays free of
/// those process-facing concerns beyond the generated manifest text itself.
pub enum InitOutcome {
    /// `--print`: the commented template, not written anywhere (R2).
    Printed(String),
    /// The manifest was written.
    Wrote { path: PathBuf, contents: String },
    /// R1: no `--print`, and stdin is not a terminal.
    NonInteractive,
    /// R4: the manifest already exists and `--force` was not passed.
    AlreadyExists(PathBuf),
    /// R5: the generated manifest failed `manifest::parse`.
    InvalidGenerated(manifest::ManifestError),
    /// R7: the user cancelled (Ctrl-C/Esc), or declined the final confirm.
    Cancelled,
    /// Any other I/O failure while prompting or writing.
    Io(io::Error),
}

/// Everything `init` asks the user, behind a trait so it can be driven by a
/// script in tests instead of a real terminal. See this module's doc comment
/// for why that is not optional here.
pub trait Prompter {
    fn intro(&mut self, title: &str) -> io::Result<()>;
    /// A non-fatal note surfaced mid-flow (e.g. "answer at least one of
    /// these"), never a question.
    fn note(&mut self, message: &str) -> io::Result<()>;
    /// `default`: `Some` pre-fills the answer and lets an empty submit use
    /// it; `None` together with `required: true` re-prompts until the
    /// answer is non-empty.
    fn text(&mut self, prompt: &str, default: Option<&str>, required: bool) -> io::Result<String>;
    fn confirm(&mut self, prompt: &str, initial: bool) -> io::Result<bool>;
    /// Show the generated manifest before asking whether to write it. This
    /// is content, not prompt chrome -- `cliclack`'s own prompts write to
    /// stderr, but the manifest itself belongs on stdout, the same channel
    /// `--print` uses, so redirecting either one captures the same shape of
    /// text.
    fn preview(&mut self, contents: &str) -> io::Result<()>;
    fn outro(&mut self, message: &str) -> io::Result<()>;
}

/// The real, terminal-backed prompter. Untestable by construction (see the
/// module doc comment) -- exercised only by hand, never by `cargo test`.
pub struct CliclackPrompter;

impl Prompter for CliclackPrompter {
    fn intro(&mut self, title: &str) -> io::Result<()> {
        cliclack::intro(title)
    }

    fn note(&mut self, message: &str) -> io::Result<()> {
        cliclack::log::warning(message)
    }

    fn text(&mut self, prompt: &str, default: Option<&str>, required: bool) -> io::Result<String> {
        let mut input = cliclack::input(prompt).required(required);
        if let Some(default) = default {
            input = input.default_input(default);
        }
        input.interact()
    }

    fn confirm(&mut self, prompt: &str, initial: bool) -> io::Result<bool> {
        cliclack::confirm(prompt).initial_value(initial).interact()
    }

    fn preview(&mut self, contents: &str) -> io::Result<()> {
        print!("{contents}");
        io::stdout().flush()
    }

    fn outro(&mut self, message: &str) -> io::Result<()> {
        cliclack::outro(message)
    }
}

/// One canned answer for [`ScriptedPrompter`], consumed in the order the
/// real flow ([`run_prompts`]) asks for them.
#[derive(Debug)]
pub enum Answer {
    Text(&'static str),
    TextErr(ErrorKind),
    Confirm(bool),
    ConfirmErr(ErrorKind),
}

/// Drives [`Prompter`] from a fixed script instead of a real terminal. This
/// is what makes [`run_prompts`] and [`run`] unit-testable at all -- see the
/// module doc comment.
pub struct ScriptedPrompter {
    answers: VecDeque<Answer>,
    /// Every `preview` call this prompter received, in order -- tests assert
    /// against this instead of capturing real stdout.
    pub shown: Vec<String>,
    /// Every `outro` message this prompter received, in order -- this is
    /// where the "close by telling the user what to run next" checklist
    /// item lives, so it needs the same kind of test-visible record.
    pub outros: Vec<String>,
}

impl ScriptedPrompter {
    pub fn new(answers: Vec<Answer>) -> Self {
        ScriptedPrompter {
            answers: answers.into(),
            shown: Vec::new(),
            outros: Vec::new(),
        }
    }

    fn next_text(&mut self) -> io::Result<String> {
        match self.answers.pop_front() {
            Some(Answer::Text(s)) => Ok(s.to_string()),
            Some(Answer::TextErr(kind)) => Err(kind.into()),
            other => panic!("script expected a Text answer next, found {other:?}"),
        }
    }

    fn next_confirm(&mut self) -> io::Result<bool> {
        match self.answers.pop_front() {
            Some(Answer::Confirm(b)) => Ok(b),
            Some(Answer::ConfirmErr(kind)) => Err(kind.into()),
            other => panic!("script expected a Confirm answer next, found {other:?}"),
        }
    }
}

impl Prompter for ScriptedPrompter {
    fn intro(&mut self, _title: &str) -> io::Result<()> {
        Ok(())
    }

    fn note(&mut self, _message: &str) -> io::Result<()> {
        Ok(())
    }

    fn text(
        &mut self,
        _prompt: &str,
        _default: Option<&str>,
        _required: bool,
    ) -> io::Result<String> {
        self.next_text()
    }

    fn confirm(&mut self, _prompt: &str, _initial: bool) -> io::Result<bool> {
        self.next_confirm()
    }

    fn preview(&mut self, contents: &str) -> io::Result<()> {
        self.shown.push(contents.to_string());
        Ok(())
    }

    fn outro(&mut self, message: &str) -> io::Result<()> {
        self.outros.push(message.to_string());
        Ok(())
    }
}

/// R3: a static placeholder, never a detected path.
const PLACEHOLDER_SOURCE: &str = ".";

/// Renders the manifest text: comments are the documentation (see
/// `crates/agent-grafter/agrafter.toml`), so this is the one place that
/// shape is written, shared by both `--print`'s fixed placeholders and a
/// real interactive answer.
fn render_manifest(
    source: &str,
    copy: &[String],
    link: &[String],
    targets: &[(String, String)],
) -> String {
    let mut out = String::new();
    out.push_str(
        "# Generated by `agent-grafter init`. Edit freely -- these comments explain\n\
         # each field. Run `agent-grafter apply --dry-run` to preview what this\n\
         # manifest would do before it touches anything.\n\n",
    );

    out.push_str("# Where your declared entries live, relative to this file.\n");
    out.push_str(&format!("source = {source:?}\n\n"));

    out.push_str(
        "# Copied into each target -- safe for entries a shared volume might not\n\
         # always be mounted, or that must not be silently shared.\n",
    );
    out.push_str(&format!("copy = {copy:?}\n\n"));

    out.push_str("# Linked into each target: one copy, read and written by every target.\n");
    out.push_str(&format!("link = {link:?}\n"));

    for (label, dest) in targets {
        out.push_str("\n# Target names are arbitrary labels you choose.\n");
        out.push_str(&format!("[targets.{label:?}]\n"));
        out.push_str(&format!("dest = {dest:?}\n"));
    }

    out
}

/// The fixed `--print` template: static placeholder values (R3), not
/// anything discovered on disk.
fn placeholder_manifest() -> String {
    render_manifest(
        PLACEHOLDER_SOURCE,
        &["copy-me.md".to_string()],
        &["memory/".to_string()],
        &[("example".to_string(), "~/.example".to_string())],
    )
}

/// Split a comma-separated answer into trimmed, non-empty entries. Blank
/// segments (an empty answer, a trailing comma, stray whitespace) are
/// dropped rather than turned into an empty-string manifest entry, which
/// `manifest::validate_entry` would refuse anyway as naming the whole source
/// directory.
fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The answers [`run_prompts`] collects, before they are rendered into TOML
/// by [`render_manifest`].
struct PromptedManifest {
    source: String,
    copy: Vec<String>,
    link: Vec<String>,
    targets: Vec<(String, String)>,
}

/// R8's question set: source directory, then `copy`/`link` (re-asked as a
/// pair until at least one is non-empty), then one or more targets (a
/// mandatory first one, then "add another?" defaulting to No).
fn run_prompts(prompter: &mut dyn Prompter) -> io::Result<PromptedManifest> {
    prompter.intro("agent-grafter init")?;

    let source = prompter.text(
        "Source directory (relative to this file)",
        Some(PLACEHOLDER_SOURCE),
        false,
    )?;

    let (copy, link) = loop {
        let copy_raw = prompter.text(
            "Files/directories to copy (comma-separated, may be empty)",
            None,
            false,
        )?;
        let link_raw = prompter.text(
            "Files/directories to link (comma-separated, may be empty)",
            None,
            false,
        )?;
        let copy = split_csv(&copy_raw);
        let link = split_csv(&link_raw);
        if !copy.is_empty() || !link.is_empty() {
            break (copy, link);
        }
        prompter.note("declare at least one `copy` or `link` entry")?;
    };

    let mut targets = Vec::new();
    loop {
        let label = prompter.text("Target name", None, true)?;
        let dest = prompter.text("Target destination path", None, true)?;
        targets.push((label, dest));

        if !prompter.confirm("Add another target?", false)? {
            break;
        }
    }

    Ok(PromptedManifest {
        source,
        copy,
        link,
        targets,
    })
}

/// Maps a `Prompter` I/O error to the outcome it means: `NotConnected` is
/// `cliclack`'s own off-terminal error (belt and braces alongside the
/// structural `is_tty` check in [`run`] -- R1), `Interrupted` is a real
/// Ctrl-C/Esc cancellation (R7), anything else is a genuine failure.
fn map_prompt_error(e: io::Error) -> InitOutcome {
    match e.kind() {
        ErrorKind::Interrupted => InitOutcome::Cancelled,
        ErrorKind::NotConnected => InitOutcome::NonInteractive,
        _ => InitOutcome::Io(e),
    }
}

/// The whole decision path, independent of `clap` and of the real terminal:
/// `is_tty` and `prompter` are both supplied by the caller so tests can
/// drive this directly (`is_tty: true` plus a [`ScriptedPrompter`]) without
/// touching a real terminal at all.
pub fn run(args: &InitArgs, is_tty: bool, prompter: &mut dyn Prompter) -> InitOutcome {
    if args.print {
        // R2: never prompts, never touches the manifest path.
        let contents = placeholder_manifest();
        return match manifest::parse(&contents) {
            Ok(_) => InitOutcome::Printed(contents),
            Err(e) => InitOutcome::InvalidGenerated(e),
        };
    }

    if !is_tty {
        // R1, checked structurally before any prompt is constructed.
        return InitOutcome::NonInteractive;
    }

    if !args.force && args.manifest_path.exists() {
        // R4: before any prompt, so a user is never walked through the
        // whole question set only to be refused at the very end.
        return InitOutcome::AlreadyExists(args.manifest_path.clone());
    }

    let prompted = match run_prompts(prompter) {
        Ok(v) => v,
        Err(e) => return map_prompt_error(e),
    };

    let contents = render_manifest(
        &prompted.source,
        &prompted.copy,
        &prompted.link,
        &prompted.targets,
    );
    if let Err(e) = manifest::parse(&contents) {
        // R5: nothing is written when the generated manifest is invalid.
        return InitOutcome::InvalidGenerated(e);
    }

    if let Err(e) = prompter.preview(&contents) {
        return map_prompt_error(e);
    }

    // R6: default Yes creating a new file, default No when --force would
    // overwrite one -- Global Constraint 4, never inherit a destructive
    // default.
    let default_confirm = !args.force;
    let confirmed = match prompter.confirm("Write this manifest?", default_confirm) {
        Ok(b) => b,
        Err(e) => return map_prompt_error(e),
    };
    if !confirmed {
        // R7: an explicit decline is nothing-written, same as a Ctrl-C
        // cancellation.
        return InitOutcome::Cancelled;
    }

    // R4: `create_new(true)` closes the check-then-write race the earlier
    // `.exists()` check above cannot, on its own, rule out. `--force` skips
    // that race protection deliberately -- overwriting is exactly what it
    // asked for.
    let write_result = if args.force {
        std::fs::write(&args.manifest_path, &contents)
    } else {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&args.manifest_path)
            .and_then(|mut f| f.write_all(contents.as_bytes()))
    };

    match write_result {
        Ok(()) => {
            // Closes the visual session `intro` opened, and is where the
            // checklist's "close by telling the user what to run next"
            // lives -- behind the trait, like everything else here, so it
            // is asserted in a test rather than only ever eyeballed. Its
            // own failure does not change the outcome: the manifest is
            // already written by this point, and a chrome-closing hiccup
            // must not report a write that happened as an error.
            let _ = prompter
                .outro("Run `agent-grafter apply --dry-run` next to preview what it would do.");
            InitOutcome::Wrote {
                path: args.manifest_path.clone(),
                contents,
            }
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            InitOutcome::AlreadyExists(args.manifest_path.clone())
        }
        Err(e) => InitOutcome::Io(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(manifest_path: PathBuf, force: bool, print: bool) -> InitArgs {
        InitArgs {
            manifest_path,
            force,
            print,
        }
    }

    #[test]
    fn print_never_prompts_and_never_touches_the_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        // An empty script: if `run` prompted even once here, `next_text`
        // or `next_confirm` would panic on an empty queue.
        let mut prompter = ScriptedPrompter::new(vec![]);

        let outcome = run(&args(path.clone(), false, true), false, &mut prompter);

        assert!(matches!(outcome, InitOutcome::Printed(_)));
        assert!(!path.exists(), "--print must not write the manifest");
    }

    #[test]
    fn print_output_parses_as_a_valid_manifest() {
        let contents = placeholder_manifest();
        manifest::parse(&contents).expect("the --print template must parse");
    }

    #[test]
    fn print_output_is_a_commented_template_not_bare_keys() {
        let contents = placeholder_manifest();
        assert!(
            contents.lines().any(|l| l.trim_start().starts_with('#')),
            "the template must carry explanatory comments, got:\n{contents}"
        );
        assert!(contents.contains("source ="));
        assert!(contents.contains("copy ="));
        assert!(contents.contains("link ="));
        assert!(contents.contains("[targets."));
    }

    #[test]
    fn print_ignores_an_existing_manifest_and_does_not_overwrite_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        std::fs::write(&path, "sentinel").unwrap();
        let mut prompter = ScriptedPrompter::new(vec![]);

        let outcome = run(&args(path.clone(), false, true), false, &mut prompter);

        assert!(matches!(outcome, InitOutcome::Printed(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "sentinel");
    }

    #[test]
    fn non_tty_without_print_refuses_before_any_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        let mut prompter = ScriptedPrompter::new(vec![]);

        let outcome = run(&args(path.clone(), false, false), false, &mut prompter);

        assert!(matches!(outcome, InitOutcome::NonInteractive));
        assert!(!path.exists());
    }

    #[test]
    fn existing_manifest_without_force_is_refused_before_any_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        std::fs::write(&path, "sentinel").unwrap();
        // Empty script: reaching a single prompt call here would panic,
        // proving R4 -- the refusal happens before any question is asked.
        let mut prompter = ScriptedPrompter::new(vec![]);

        let outcome = run(&args(path.clone(), false, false), true, &mut prompter);

        assert!(matches!(outcome, InitOutcome::AlreadyExists(p) if p == path));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "sentinel");
    }

    #[test]
    fn force_bypasses_the_existing_manifest_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        std::fs::write(&path, "sentinel").unwrap();
        let mut prompter = ScriptedPrompter::new(vec![
            Answer::Text("."),            // source
            Answer::Text("notes.md"),     // copy
            Answer::Text(""),             // link
            Answer::Text("example"),      // target name
            Answer::Text("/tmp/example"), // target dest
            Answer::Confirm(false),       // add another target?
            Answer::Confirm(true),        // write this manifest?
        ]);

        let outcome = run(&args(path.clone(), true, false), true, &mut prompter);

        assert!(matches!(outcome, InitOutcome::Wrote { .. }));
        assert_ne!(std::fs::read_to_string(&path).unwrap(), "sentinel");
    }

    #[test]
    fn a_full_answered_flow_writes_the_generated_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        let mut prompter = ScriptedPrompter::new(vec![
            Answer::Text("src"),
            Answer::Text("AGENTS.md, notes/"),
            Answer::Text("memory/"),
            Answer::Text("agent-a"),
            Answer::Text("~/.agent-a"),
            Answer::Confirm(true), // add another target?
            Answer::Text("agent-b"),
            Answer::Text("~/.agent-b"),
            Answer::Confirm(false), // add another target?
            Answer::Confirm(true),  // write this manifest?
        ]);

        let outcome = run(&args(path.clone(), false, false), true, &mut prompter);

        let contents = match outcome {
            InitOutcome::Wrote { contents, .. } => contents,
            _ => panic!("expected Wrote"),
        };
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
        manifest::parse(&contents).expect("a written manifest must parse");
        assert!(contents.contains("source = \"src\""));
        assert!(contents.contains("AGENTS.md"));
        assert!(contents.contains("notes/"));
        assert!(contents.contains("memory/"));
        assert!(contents.contains("[targets.\"agent-a\"]"));
        assert!(contents.contains("[targets.\"agent-b\"]"));
        assert_eq!(
            prompter.outros.len(),
            1,
            "a successful write must close the session with exactly one outro"
        );
        assert!(
            prompter.outros[0].contains("apply --dry-run"),
            "the closing message must tell the user what to run next, got: {:?}",
            prompter.outros[0]
        );
    }

    #[test]
    fn empty_copy_and_link_is_re_asked_until_one_is_non_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        let mut prompter = ScriptedPrompter::new(vec![
            Answer::Text("."),
            Answer::Text(""),        // copy: empty
            Answer::Text(""),        // link: empty -- must re-ask, not proceed
            Answer::Text(""),        // copy again: still empty
            Answer::Text("memory/"), // link again: now non-empty
            Answer::Text("example"),
            Answer::Text("~/.example"),
            Answer::Confirm(false),
            Answer::Confirm(true),
        ]);

        let outcome = run(&args(path.clone(), false, false), true, &mut prompter);

        assert!(matches!(outcome, InitOutcome::Wrote { .. }));
    }

    #[test]
    fn generated_manifest_that_fails_validation_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        let mut prompter = ScriptedPrompter::new(vec![
            Answer::Text("."),
            Answer::Text(".env"), // a denied credential filename
            Answer::Text(""),
            Answer::Text("example"),
            Answer::Text("~/.example"),
            Answer::Confirm(false),
            // No final confirm answer: `run` must return before reaching
            // it, or `next_confirm` panics on an empty queue.
        ]);

        let outcome = run(&args(path.clone(), false, false), true, &mut prompter);

        assert!(matches!(outcome, InitOutcome::InvalidGenerated(_)));
        assert!(!path.exists());
    }

    #[test]
    fn ctrl_c_during_a_prompt_cancels_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        let mut prompter = ScriptedPrompter::new(vec![
            Answer::Text("."),
            Answer::TextErr(ErrorKind::Interrupted),
        ]);

        let outcome = run(&args(path.clone(), false, false), true, &mut prompter);

        assert!(matches!(outcome, InitOutcome::Cancelled));
        assert!(!path.exists());
    }

    #[test]
    fn declining_the_final_confirm_cancels_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        let mut prompter = ScriptedPrompter::new(vec![
            Answer::Text("."),
            Answer::Text("notes.md"),
            Answer::Text(""),
            Answer::Text("example"),
            Answer::Text("~/.example"),
            Answer::Confirm(false), // add another target?
            Answer::Confirm(false), // write this manifest? -- declined
        ]);

        let outcome = run(&args(path.clone(), false, false), true, &mut prompter);

        assert!(matches!(outcome, InitOutcome::Cancelled));
        assert!(!path.exists());
        assert!(
            prompter.outros.is_empty(),
            "a declined write must not close the session as if it had succeeded"
        );
    }

    #[test]
    fn a_not_connected_prompt_error_mid_flow_reports_non_interactive() {
        // Belt-and-braces (R1): even though `is_tty: true` was asserted by
        // the caller, a `cliclack` prompt can still fail this way (e.g. a
        // race between the check and the prompt). It must map to the same
        // outcome as the structural check, not to a generic failure.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        let mut prompter = ScriptedPrompter::new(vec![Answer::TextErr(ErrorKind::NotConnected)]);

        let outcome = run(&args(path.clone(), false, false), true, &mut prompter);

        assert!(matches!(outcome, InitOutcome::NonInteractive));
        assert!(!path.exists());
    }

    #[test]
    fn preview_shows_the_generated_manifest_before_the_final_confirm() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-grafter.toml");
        let mut prompter = ScriptedPrompter::new(vec![
            Answer::Text("."),
            Answer::Text("notes.md"),
            Answer::Text(""),
            Answer::Text("example"),
            Answer::Text("~/.example"),
            Answer::Confirm(false),
            Answer::Confirm(true),
        ]);

        run(&args(path.clone(), false, false), true, &mut prompter);

        assert_eq!(prompter.shown.len(), 1);
        assert!(prompter.shown[0].contains("notes.md"));
    }

    #[test]
    fn a_default_new_file_confirm_defaults_to_yes_and_force_defaults_to_no() {
        // `Confirm(initial)` below is what the *script* answers, which is
        // independent of the `initial` value `run` passes to the trait --
        // this test instead pins the argument via a thin recording
        // prompter, so a regression that flips R6's defaults is caught.
        struct RecordingPrompter {
            inner: ScriptedPrompter,
            confirm_initials: Vec<bool>,
        }
        impl Prompter for RecordingPrompter {
            fn intro(&mut self, t: &str) -> io::Result<()> {
                self.inner.intro(t)
            }
            fn note(&mut self, m: &str) -> io::Result<()> {
                self.inner.note(m)
            }
            fn text(&mut self, p: &str, d: Option<&str>, r: bool) -> io::Result<String> {
                self.inner.text(p, d, r)
            }
            fn confirm(&mut self, prompt: &str, initial: bool) -> io::Result<bool> {
                self.confirm_initials.push(initial);
                self.inner.confirm(prompt, initial)
            }
            fn preview(&mut self, c: &str) -> io::Result<()> {
                self.inner.preview(c)
            }
            fn outro(&mut self, m: &str) -> io::Result<()> {
                self.inner.outro(m)
            }
        }

        let dir = tempfile::tempdir().unwrap();

        let new_path = dir.path().join("new.toml");
        let mut new_prompter = RecordingPrompter {
            inner: ScriptedPrompter::new(vec![
                Answer::Text("."),
                Answer::Text("notes.md"),
                Answer::Text(""),
                Answer::Text("example"),
                Answer::Text("~/.example"),
                Answer::Confirm(false),
                Answer::Confirm(true),
            ]),
            confirm_initials: Vec::new(),
        };
        run(&args(new_path, false, false), true, &mut new_prompter);
        assert_eq!(
            new_prompter.confirm_initials.last(),
            Some(&true),
            "creating a new file must default the final confirm to Yes"
        );

        let force_path = dir.path().join("force.toml");
        std::fs::write(&force_path, "sentinel").unwrap();
        let mut force_prompter = RecordingPrompter {
            inner: ScriptedPrompter::new(vec![
                Answer::Text("."),
                Answer::Text("notes.md"),
                Answer::Text(""),
                Answer::Text("example"),
                Answer::Text("~/.example"),
                Answer::Confirm(false),
                Answer::Confirm(true),
            ]),
            confirm_initials: Vec::new(),
        };
        run(&args(force_path, true, false), true, &mut force_prompter);
        assert_eq!(
            force_prompter.confirm_initials.last(),
            Some(&false),
            "--force overwriting an existing file must default to No"
        );
    }
}
