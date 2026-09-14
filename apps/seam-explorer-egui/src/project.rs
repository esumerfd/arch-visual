//! Project-directory entry point for "Open Project" (quick task
//! 260913-gud): pick or receive a project directory, and if it already has
//! a Graphify-produced graphify-out/graph.json, hand the path to
//! startup::preload_graph -- the one ingest authority -- rather than
//! re-reading or re-parsing it here. When no graph exists yet, arm a
//! prompt (rendered by poll_and_prompt) offering to build one by running
//! graphify off the UI thread.
//!
//! This module's state lives in a process-global (OnceLock<Mutex<...>>),
//! following load.rs's and event_stream.rs's idiom, rather than as a
//! field on the frozen SeamExplorerApp -- see app.rs's module doc for
//! why that struct is frozen.

use crate::app::SeamExplorerApp;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Mutex, OnceLock};

/// The subdirectory Graphify writes its export into, relative to a project
/// directory.
const GRAPHIFY_OUT_DIR: &str = "graphify-out";
/// The graph file Graphify writes inside GRAPHIFY_OUT_DIR.
const GRAPH_FILE_NAME: &str = "graph.json";

/// Joins the Graphify output layout onto dir. Pure.
pub fn graph_path_for(dir: &Path) -> PathBuf {
    dir.join(GRAPHIFY_OUT_DIR).join(GRAPH_FILE_NAME)
}

/// The result of a graphify build run.
#[derive(Debug)]
pub enum BuildOutcome {
    Succeeded(PathBuf),
    Failed(String),
}

/// Bound on how much of a failed build's stderr a Banner carries -- it goes
/// into UI text a human reads, not a log, so only the tail matters.
const STDERR_TAIL_MAX_BYTES: usize = 400;

fn stderr_tail(stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.len() <= STDERR_TAIL_MAX_BYTES {
        return trimmed.to_string();
    }
    let start = trimmed.len() - STDERR_TAIL_MAX_BYTES;
    let mut idx = start;
    while !trimmed.is_char_boundary(idx) {
        idx += 1;
    }
    trimmed[idx..].to_string()
}

/// Returns the first candidate that exists as a file on disk, else the bare
/// tool name (the last candidate's file name) so a PATH lookup still gets
/// its chance. Pure.
pub fn program_from(candidates: &[PathBuf]) -> String {
    for candidate in candidates {
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    candidates
        .last()
        .and_then(|c| c.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Resolves the graphify binary to run. A macOS .app launched from Finder
/// or `open` inherits the launchd environment, not the login shell's PATH,
/// so the directory that actually holds graphify (typically
/// ~/.local/bin) is usually absent there -- a bare-name spawn then fails
/// with NotFound for a user who can run graphify fine in a terminal.
/// These absolute fallbacks are what make a bundled .app work without the
/// user having to symlink graphify onto a system path.
pub fn graphify_program() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        PathBuf::from(format!("{home}/.local/bin/graphify")),
        PathBuf::from("/opt/homebrew/bin/graphify"),
        PathBuf::from("/usr/local/bin/graphify"),
    ];
    program_from(&candidates)
}

/// Builds the headless, code-only extract invocation: [program, extract,
/// dir, --code-only]. An argument vector, never a shell string -- a
/// directory name containing spaces or shell metacharacters is then inert
/// by construction, matching open_file.rs's build_command rule.
pub fn build_command(dir: &Path) -> Vec<String> {
    vec![
        graphify_program(),
        "extract".to_string(),
        dir.to_string_lossy().into_owned(),
        "--code-only".to_string(),
    ]
}

/// Pure decision table: succeeded only when the process exited cleanly AND
/// the expected file is now on disk. A clean exit with no file is Failed,
/// carrying wording about the tool finishing without producing a graph,
/// plus the stderr tail if there is one.
pub fn classify_build(exit_ok: bool, stderr: &str, graph_written: bool) -> BuildOutcome {
    if exit_ok && graph_written {
        return BuildOutcome::Succeeded(PathBuf::new());
    }
    let tail = stderr_tail(stderr);
    let message = if exit_ok {
        if tail.is_empty() {
            "graphify finished without producing a graph.json file.".to_string()
        } else {
            format!("graphify finished without producing a graph.json file. {tail}")
        }
    } else if tail.is_empty() {
        "graphify exited with an error.".to_string()
    } else {
        format!("graphify exited with an error: {tail}")
    };
    BuildOutcome::Failed(message)
}

/// Synchronous: runs argv, captures output, and hands the three facts to
/// classify_build. A spawn failure (the NotFound case graphify_program's
/// doc explains) is itself a Failed whose message names the resolved
/// program, so the banner tells the user which binary was not found rather
/// than just "failed".
pub fn run_build(argv: &[String], graph_path: &Path) -> BuildOutcome {
    let Some((program, args)) = argv.split_first() else {
        return BuildOutcome::Failed("no program to run".to_string());
    };
    match std::process::Command::new(program).args(args).output() {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let graph_written = graph_path.is_file();
            match classify_build(output.status.success(), &stderr, graph_written) {
                BuildOutcome::Succeeded(_) => BuildOutcome::Succeeded(graph_path.to_path_buf()),
                other => other,
            }
        }
        Err(e) => BuildOutcome::Failed(format!("could not run {program}: {e}")),
    }
}

/// Builds the naming invocation: [program, label, dir]. Resolves through
/// the same graphify_program() the extract step uses -- never a second bare
/// tool-name literal -- so both invocations agree on which binary is run.
pub fn label_command(dir: &Path) -> Vec<String> {
    vec![
        graphify_program(),
        "label".to_string(),
        dir.to_string_lossy().into_owned(),
    ]
}

/// The result of a naming run. A naming failure is always survivable (D-02):
/// by the time this runs, the extract step has already written a loadable
/// graph, so a failed naming step never means there is nothing to show.
#[derive(Debug)]
pub enum LabelOutcome {
    Labeled,
    Skipped(String),
}

/// Pure decision table, the mirror of classify_build: a clean exit is
/// Labeled; anything else is Skipped, carrying the stderr tail when there is
/// one and a generic sentence about the naming step not completing when
/// there is not. There is deliberately no "graph missing" input here -- by
/// the time this runs the graph already exists, which is the whole reason a
/// failure is survivable.
pub fn classify_label(exit_ok: bool, stderr: &str) -> LabelOutcome {
    if exit_ok {
        return LabelOutcome::Labeled;
    }
    let tail = stderr_tail(stderr);
    let reason = if tail.is_empty() {
        "the naming step did not complete.".to_string()
    } else {
        tail
    };
    LabelOutcome::Skipped(reason)
}

/// Synchronous, the mirror of run_build: runs argv, captures output, and
/// hands the exit status and stderr to classify_label. A spawn failure is
/// itself a Skipped naming the resolved program, so the note tells the user
/// which binary was missing. Touches the process environment in no way --
/// inheriting whatever backend the user already configured is exactly D-01's
/// mechanism, and scrubbing or injecting env vars here would break it.
pub fn run_label(argv: &[String]) -> LabelOutcome {
    let Some((program, args)) = argv.split_first() else {
        return LabelOutcome::Skipped("no program to run".to_string());
    };
    match std::process::Command::new(program).args(args).output() {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            classify_label(output.status.success(), &stderr)
        }
        Err(e) => LabelOutcome::Skipped(format!("could not run {program}: {e}")),
    }
}

/// The outcome of a full project build: the extract step's outcome, plus
/// what happened to the naming step. `label: None` means naming was never
/// attempted because the extract step produced no graph to name -- not that
/// naming silently succeeded.
#[derive(Debug)]
pub struct BuildReport {
    pub outcome: BuildOutcome,
    pub label: Option<LabelOutcome>,
}

/// The sequential two-step (D-01): run_build first; on Failed, return
/// immediately with label: None and do not spawn anything else; on
/// Succeeded, run run_label and return both verdicts. Taking both argument
/// vectors as parameters rather than deriving them inside is what lets
/// tests drive this with system binaries instead of graphify.
pub fn run_project_build(
    build_argv: &[String],
    label_argv: &[String],
    graph_path: &Path,
) -> BuildReport {
    let outcome = run_build(build_argv, graph_path);
    match outcome {
        BuildOutcome::Succeeded(path) => {
            let label = run_label(label_argv);
            BuildReport {
                outcome: BuildOutcome::Succeeded(path),
                label: Some(label),
            }
        }
        failed @ BuildOutcome::Failed(_) => BuildReport {
            outcome: failed,
            label: None,
        },
    }
}

/// Maps a build failure into the same error-kind Banner shape load.rs's
/// error_banner renders, naming graphify explicitly.
pub fn build_error_banner(msg: &str) -> crate::app::Banner {
    crate::app::Banner {
        kind: crate::app::BannerKind::Error,
        heading: "Couldn't build this project's graph".to_string(),
        body: format!("Running graphify failed: {msg}"),
    }
}

/// Runs argv on a spawned thread, sending the outcome down an mpsc channel
/// and waking ctx so the UI repaints even if the user hasn't touched the
/// mouse for the whole extraction. Send first, then request the repaint,
/// so the frame that wakes is guaranteed to find the outcome waiting.
/// Mirrors event_stream::spawn_receiver's shape deliberately, not a second
/// invented one.
pub fn spawn_build(
    build_argv: Vec<String>,
    label_argv: Vec<String>,
    graph_path: PathBuf,
    ctx: egui::Context,
) -> Receiver<BuildReport> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let report = run_project_build(&build_argv, &label_argv, &graph_path);
        let _ = tx.send(report);
        ctx.request_repaint();
    });
    rx
}

/// Applies a finished project build to app state: on success, loads the
/// written graph through the one ingest authority (startup::preload_graph);
/// on failure, sets an error banner naming graphify and the failure detail.
/// A naming outcome of Skipped never turns a loaded graph into a failure
/// (D-02) -- Task 2 adds the informational banner explaining the skip.
pub fn apply_build_report(app: &mut SeamExplorerApp, report: BuildReport) {
    match report.outcome {
        BuildOutcome::Succeeded(path) => {
            crate::startup::preload_graph(app, &path);
        }
        BuildOutcome::Failed(msg) => {
            app.banner = Some(build_error_banner(&msg));
        }
    }
}

/// This module's process-global state. Matched exhaustively everywhere --
/// no catch-all arm -- so the compiler finds every site a new variant
/// needs to touch.
enum ProjectState {
    Idle,
    Prompting(PathBuf),
    Building {
        dir: PathBuf,
        rx: Receiver<BuildReport>,
    },
}

static PROJECT_STATE: OnceLock<Mutex<ProjectState>> = OnceLock::new();

fn project_state_lock() -> &'static Mutex<ProjectState> {
    PROJECT_STATE.get_or_init(|| Mutex::new(ProjectState::Idle))
}

/// The directory a "no graph found here" prompt is currently armed for, or
/// None if nothing is pending (including while a build is running -- that
/// is a distinct state, not a pending prompt).
pub fn pending_prompt_dir() -> Option<PathBuf> {
    let guard = project_state_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match &*guard {
        ProjectState::Idle => None,
        ProjectState::Prompting(dir) => Some(dir.clone()),
        ProjectState::Building { .. } => None,
    }
}

/// Resets this module's state to Idle. Used by tests to isolate cases
/// sharing the process-global.
pub fn clear_pending() {
    let mut guard = project_state_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *guard = ProjectState::Idle;
}

/// If dir already has a Graphify graph, loads it through the one ingest
/// authority (startup::preload_graph) and clears any pending prompt.
/// Otherwise arms the "no graph here" prompt for dir and leaves app
/// untouched. Contains no read, no parse, and no ingest of its own -- the
/// error banner, the dropped-edge warning banner, and the
/// remember_graph_path capture all come from the one existing load path.
pub fn open_project_dir(app: &mut SeamExplorerApp, dir: &Path) {
    let path = graph_path_for(dir);
    if path.is_file() {
        crate::startup::preload_graph(app, &path);
        let mut guard = project_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = ProjectState::Idle;
    } else {
        let mut guard = project_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = ProjectState::Prompting(dir.to_path_buf());
    }
}

/// Native "choose a project directory" dialog. Cancel is silent, matching
/// load::pick_file's cancel semantics. The only impure entry point in
/// this module, and the only function here the tests cannot drive.
pub fn open_project(app: &mut SeamExplorerApp) {
    let Some(dir) = rfd::FileDialog::new().pick_folder() else {
        return;
    };
    open_project_dir(app, &dir);
}

/// The one per-frame entry point for this module's prompt/build UI. Takes
/// the current state OUT of the global with std::mem::replace to Idle,
/// acts on it (which may render UI or spawn a build), then writes back
/// whatever state should hold next -- the lock is never held across a UI
/// callback or across preload_graph.
pub fn poll_and_prompt(ctx: &egui::Context, app: &mut SeamExplorerApp) {
    let state = {
        let mut guard = project_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::replace(&mut *guard, ProjectState::Idle)
    };

    match state {
        ProjectState::Idle => {}
        ProjectState::Prompting(dir) => {
            let mut cancelled = false;
            let mut start_build = false;
            egui::Window::new("Open Project")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("No graph was found in {}.", dir.display()));
                    ui.label(
                        "Build one now? This runs graphify to extract the project, then \
                         names its communities using whatever LLM backend is already \
                         configured on this machine. This may take a while on a large \
                         project.",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Build graph").clicked() {
                            start_build = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancelled = true;
                        }
                    });
                });

            if start_build {
                let build_argv = build_command(&dir);
                let label_argv = label_command(&dir);
                let graph_path = graph_path_for(&dir);
                let rx = spawn_build(build_argv, label_argv, graph_path, ctx.clone());
                let mut guard = project_state_lock()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *guard = ProjectState::Building { dir, rx };
            } else if !cancelled {
                let mut guard = project_state_lock()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *guard = ProjectState::Prompting(dir);
            }
            // cancelled: leave Idle (already set by the replace above).
        }
        ProjectState::Building { dir, rx } => {
            egui::Window::new("Building project graph")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label(format!(
                            "Processing {}: extracting, then naming communities...",
                            dir.display()
                        ));
                    });
                });

            match rx.try_recv() {
                Ok(report) => {
                    apply_build_report(app, report);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint();
                    let mut guard = project_state_lock()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    *guard = ProjectState::Building { dir, rx };
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    app.banner = Some(build_error_banner("the build process ended unexpectedly"));
                }
            }
        }
    }
}
