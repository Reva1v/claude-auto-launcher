//! (local) The launcher's two screenless entries for claude-tg-bridge (`launcher_api`): the reads
//! each needs. `--launcher-run` then goes through `launch()` like a screen's outcome.

use crate::config::{Config, PathCtx};
use crate::frame::Style;
use crate::launch::screen::LaunchView;
use crate::launch::{LaunchState, Roster};
use crate::launcher_api::{self, RunRequest};
use crate::types::VersionInfo;
use crate::{env, maintenance, out, prefs};

use super::screens::launch_view;
use super::{UiOutcome, var};

/// Nothing is drawn; the launch reads only the limits and the resolved defaults of the view.
const NOT_DRAWN: Style = Style { color: false, ascii: true };

/// What a checked `--launcher-run` hands to the launch.
#[derive(Debug, Clone)]
pub(super) struct LauncherRun {
    state: LaunchState,
    resume_id: Option<String>,
    fork: bool,
    view: LaunchView,
}

impl LauncherRun {
    pub(super) fn project(&self) -> &str {
        &self.state.project
    }

    pub(super) fn outcome(&self) -> UiOutcome {
        UiOutcome {
            state: self.state.clone(),
            resume_id: self.resume_id.clone(),
            // The bridge names the account the transcript lives in, so nothing crosses accounts.
            transfer: None,
            fork: self.fork,
            view: self.view.clone(),
            restored: Vec::new(),
            project_source: "launcher-run".to_owned(),
            preselect: self.state.project.clone(),
            chosen_how: Some("launcher-run".to_owned()),
        }
    }
}

fn read_prefs(ctx: &PathCtx) -> prefs::Prefs {
    prefs::read_prefs(&prefs::prefs_path(ctx, var("CLAUDE_AUTO_PREFS").as_deref()))
}

/// The catalog cache and nothing more: a cold cache means a rescan of a 200 MB binary and a write,
/// and neither entry may pay for what only the screen's labels want. `true` is that mode of the
/// catalog; it must run before the view, whose defaults are named through it.
fn load_catalog() -> Option<std::collections::BTreeMap<String, String>> {
    env::model_family_labels(true)
}

/// Checks the request; `Err` is the refusal, before anything is written, logged or started.
pub(super) fn prepare_run(req: &RunRequest, ctx: &PathCtx, cfg: &Config, roster: &Roster) -> Result<LauncherRun, String> {
    let mut req = req.clone();
    req.project = ctx.expand(&req.project);
    load_catalog();
    let view = launch_view(cfg, ctx, VersionInfo::default(), NOT_DRAWN);
    let state = launcher_api::resolve_run(&req, roster, &view.limits, &read_prefs(ctx))?;
    Ok(LauncherRun {
        state,
        resume_id: req.resume_id,
        fork: req.fork,
        view,
    })
}

/// `--launcher-options`: one line of JSON on stdout. Reads files only: no process, no network, no
/// write of any kind. The bridge parses this line, so `out::raw` rather than a painted line.
pub(super) fn print_options(ctx: &PathCtx, cfg: &Config, roster: &Roster) -> i32 {
    let mut roster = roster.clone();
    if let Some(labels) = load_catalog() {
        roster.set_model_labels(&labels);
    }
    let view = launch_view(cfg, ctx, VersionInfo::default(), NOT_DRAWN);
    let version = maintenance::newest_build();
    let body = launcher_api::options_json(
        &roster,
        &cfg.accounts,
        &view.limits,
        &view.labels_by_account,
        &read_prefs(ctx),
        version.as_deref(),
    );
    out::raw(&format!("{body}\n"));
    0
}
