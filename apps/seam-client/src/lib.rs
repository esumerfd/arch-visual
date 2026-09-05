//! `seam-client` -- the Claude Code `PostToolUse` hook binary's guts.
//!
//! The whole crate is one path: read the hook's stdin JSON, decide whether
//! the edit changed anything structural, and if so put exactly one
//! [`seam_core::GraphEvent`] on the datagram socket the running app binds.
//!
//! Three structural rules hold across every module here, not just the one
//! they are written next to:
//!
//! 1. **Nothing panics on stdin-derived data.** No `unwrap`, no `expect`, no
//!    slice indexing, no arithmetic that can overflow. A hook that crashes
//!    inside the user's live editing session is a hook the user turns off.
//! 2. **Nothing is printed, ever.** Not a diagnostic, not a warning. Anything
//!    written here lands in the user's real session transcript.
//! 3. **The file on disk is never read.** The hook's own payload carries the
//!    before and after text verbatim, so re-reading the edited path would
//!    only add a race the payload does not have and cost startup time the
//!    latency budget cannot spare.
//!
//! This crate deliberately holds no graph structure of any kind. Per D-03 it
//! ADVERTISES what changed and never resolves graph semantics; D-04 puts
//! resolution on the receiving side, where a live `Model` exists.

pub mod detect;
pub mod hook_input;
pub mod send;
