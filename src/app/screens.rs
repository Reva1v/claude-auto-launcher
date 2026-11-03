//! The UI sequence of one launch: launch screen, project screen, held-session warning, picker.

use std::path::{Path, PathBuf};

use crate::config::{Config, PathCtx};
use crate::frame::{self, Style};
use crate::input::console::ConsoleInput;
use crate::input::{Event, KeyPress, is_hotkey, vk};
use crate::launch::LaunchState;
use crate::launch::launch_mode;
use crate::launch::screen::{LaunchScreen, LaunchView};
use crate::out;
use crate::prefs;
use crate::timing::view;
use crate::types::VersionInfo;
use crate::ui::{ConsoleTerm, PreviewTerm, Term, UiStop, parse_preview_keys, parse_preview_size};
use crate::{env, maintenance, projects, sessions, sys, theme};

use super::{Run, UiEnd, UiOutcome, now_ms, var};

/// The UI: launch screen, project screen, held-session warning, picker - until a launch.
#[expect(clippy::too_many_lines, reason = "the screen sequence reads top to bottom as in claude-auto.ps1")]
pub(super) fn run_ui(run: &Run, input_redirected: bool, output_redirected: bool) -> Result<UiEnd, UiStop> {
    let style = Style {
        color: theme::color_supported(var("NO_COLOR").as_deref(), var("TERM").as_deref()),
        ascii: theme::ascii_required(var("CLAUDE_AUTO_ASCII").as_deref(), sys::output_code_page()),
    };
    let tm = &run.logger.timings;
    tm.set_style(style.color, style.ascii);
    let load = tm.begin(&["load", "launch"]);
    let mut version: VersionInfo = maintenance::read_version();
    if !run.preview && var("CLAUDE_AUTO_NO_UPDATE").as_deref() != Some("1") {
        let _s = tm.scope(&["update check"]);
        maintenance::update_before_launch(&mut version);
    }
    let mut roster = run.roster.clone();
    let labels = {
        let _s = tm.scope(&["model catalog"]);
        env::model_family_labels(run.preview)
    };
    if let Some(labels) = labels {
        roster.set_model_labels(&labels);
    }
    let accounts = tm.begin(&["accounts"]);
    let view = launch_view(&run.cfg, &run.ctx, version, style);
    tm.end(accounts);
    let console = tm.begin(&["console"]);
    let mut term: Box<dyn Term> = if run.preview {
        let keys = parse_preview_keys(&var("CLAUDE_AUTO_PREVIEW_KEYS").unwrap_or_default()).map_err(|e| {
            out::err_line(&e);
            UiStop::KeysExhausted
        })?;
        Box::new(
            PreviewTerm::new(parse_preview_size(var("CLAUDE_AUTO_PREVIEW_SIZE").as_deref()), keys, false).with_logger(run.logger.clone()),
        )
    } else {
        // (local) Keyboard only on this machine: CLAUDE_AUTO_MOUSE=1 arms the mouse again.
        let mouse = var("CLAUDE_AUTO_MOUSE").as_deref() == Some("1") && var("CLAUDE_AUTO_NO_MOUSE").as_deref() != Some("1");
        let trace = (var("CLAUDE_AUTO_INPUT_TRACE").as_deref() == Some("1")).then(|| trace_path(&run.ctx));
        let Some(input) = ConsoleInput::open(mouse, output_redirected, trace) else {
            return Ok(UiEnd::NoConsole);
        };
        let alt = !output_redirected && !input_redirected;
        Box::new(ConsoleTerm::open(alt, Some(input), run.logger.clone()))
    };

    tm.end(console);
    let prefs_span = tm.begin(&["prefs"]);
    let prefs_file = prefs::read_prefs(&prefs::prefs_path(&run.ctx, var("CLAUDE_AUTO_PREFS").as_deref()));
    let mut state = roster.new_state();
    let merged = prefs::merge_prefs(&mut state, &prefs_file, &roster, now_ms());
    tm.end(prefs_span);
    tm.end(load);
    let load = tm.begin(&["load", "project"]);
    let preview = run.preview;
    if preview {
        projects::remove_stale_preview_cache(&std::env::temp_dir());
    }
    let account_roots: Vec<String> = run.cfg.accounts.iter().map(|a| a.root.clone()).collect();
    let roots = projects::launcher_profile_roots(&run.ctx.home, var("CLAUDE_CONFIG_DIR").as_deref(), &account_roots);
    // (local) Built beside the launch screen, which shows no project on this machine, and collected
    // once it is left. The start rule then runs on the account the owner ended on.
    let registry = std::thread::spawn(move || projects::all_accounts_registry(&account_roots, &roots, preview));
    let mut registry = Some(registry);
    let mut project_list: Vec<projects::Project> = Vec::new();
    let mut project_source = String::new();
    let mut preselect = String::new();
    tm.end(load);
    let held_dirs: Vec<PathBuf> = run.cfg.accounts.iter().map(|a| Path::new(&a.root).join("sessions")).collect();
    let mut chosen_how: Option<String>;
    let actions = run.cfg.maintenance_actions.clone();
    // (local) No project screen unless asked: Enter starts where the launcher was opened.
    let always_project_screen = var("CLAUDE_AUTO_PROJECT_SCREEN").as_deref() == Some("1");

    let outcome = loop {
        let mut hook = |k: &KeyPress, t: &mut dyn Term| -> bool {
            if is_hotkey(k, 'u') {
                if maintenance::run_screen(t, &actions, style).is_err() {
                    return true;
                }
                t.drain();
                return true;
            }
            false
        };
        let mut screen = LaunchScreen::new(&roster, state.clone(), &view, &prefs_file, now_ms(), &mut hook);
        let Some(next) = crate::ui::screen_loop::run(&mut screen, term.as_mut())? else {
            break UiEnd::Cancelled;
        };
        state = next;
        if let Some(task) = registry.take() {
            let wait = tm.begin(&["load", "project"]);
            project_list = task.join().unwrap_or_default();
            project_source = projects::set_launch_start_project(&mut state, &run.ctx.cwd, &project_list);
            preselect.clone_from(&state.project);
            tm.end(wait);
        }
        // (local) The 'project' action opens the project screen for this one launch; Escape from
        // anything below it brings the launch row back to 'project'.
        let via_project = state.action == "project";
        if always_project_screen || via_project {
            let initial = if via_project { "new" } else { state.action.as_str() };
            let Some(chosen) = projects::run_project_screen(term.as_mut(), &project_list, &run.ctx.cwd, initial, style)? else {
                if run.preview {
                    break UiEnd::PreviewPickerCancelled;
                }
                continue;
            };
            state.project.clone_from(&chosen.path);
            state.project_slug.clone_from(&chosen.slug);
            state.project_slugs.clone_from(&chosen.slugs);
            state.action.clone_from(&chosen.action);
            chosen_how = Some(chosen.how);
        } else {
            state.project = run.ctx.cwd.to_string_lossy().into_owned();
            state.project_slugs = projects::slugs_for(&project_list, &run.ctx.cwd);
            state.project_slug = state.project_slugs.first().cloned().unwrap_or_default();
            chosen_how = Some("cwd".to_owned());
        }
        let mode = launch_mode(&state.action);
        let mut slugs: Vec<String> = state.project_slugs.iter().filter(|s| !s.is_empty()).cloned().collect();
        if slugs.is_empty() && !state.project_slug.is_empty() {
            slugs.push(state.project_slug.clone());
        }
        let sessions_root = sessions::sessions_root(&run.cfg, &state.account);
        if mode.is_some_and(|m| m.attaches_newest) {
            let holder = {
                let _s = tm.scope(&["held check"]);
                sessions::continue_session_holder(&held_dirs, &slugs, &sessions_root)
            };
            if confirm_held(term.as_mut(), holder.as_deref(), style)? {
                break launch_end(
                    state,
                    None,
                    None,
                    false,
                    view,
                    merged.restored,
                    project_source,
                    preselect,
                    chosen_how,
                );
            }
            if run.preview {
                break UiEnd::PreviewHeldBack;
            }
            if via_project {
                "project".clone_into(&mut state.action);
            }
            continue;
        }
        if !mode.is_some_and(|m| m.picks_session) {
            break launch_end(
                state,
                None,
                None,
                false,
                view,
                merged.restored,
                project_source,
                preselect,
                chosen_how,
            );
        }
        let name = Path::new(&state.project)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let others = sessions::other_stores(&run.cfg, &state.account);
        let req = sessions::PickerRequest {
            sessions_root,
            others: &others,
            project_slugs: &slugs,
            project_name: &name,
            style,
            preview: run.preview,
        };
        if let Some(picked) = sessions::run_picker(term.as_mut(), &req)? {
            let holder = if picked.fork {
                None
            } else {
                let _s = tm.scope(&["held check"]);
                sessions::session_holder(&held_dirs, &picked.session_id)
            };
            if confirm_held(term.as_mut(), holder.as_deref(), style)? {
                break launch_end(
                    state,
                    Some(picked.session_id),
                    picked.transfer,
                    picked.fork,
                    view,
                    merged.restored,
                    project_source,
                    preselect,
                    chosen_how,
                );
            }
            if run.preview {
                break UiEnd::PreviewHeldBack;
            }
            if via_project {
                "project".clone_into(&mut state.action);
            }
            continue;
        }
        if run.preview {
            break UiEnd::PreviewPickerCancelled;
        }
        if via_project {
            "project".clone_into(&mut state.action);
        }
    };
    if preview && let Some(task) = registry.take() {
        // Left before the registry was collected: its per-PID preview caches are swept below.
        drop(task.join());
    }
    if preview {
        projects::remove_own_preview_caches(&std::env::temp_dir());
    }
    Ok(outcome)
}

/// What the screen is drawn from besides the state: limits and resolved defaults per account. Reads
/// files only; (local) `--launcher-options` and `--launcher-run` read the same.
pub(super) fn launch_view(cfg: &Config, ctx: &PathCtx, version: VersionInfo, style: Style) -> LaunchView {
    let canonical_root = cfg.canonical().map(|a| a.root.clone()).unwrap_or_default();
    let settings = |root: &str| Path::new(root).join("settings.json");
    let rate_dir = var("CLAUDE_AUTO_RATE_LIMITS_DIR").map_or_else(|| ctx.home.join(".claude").join("rate-limits"), PathBuf::from);
    LaunchView {
        limits: env::rate_limit_summary(&cfg.accounts, &rate_dir, now_ms()),
        version,
        default_model_label: env::default_model_label(&settings(&canonical_root)),
        default_advisor_label: env::default_advisor_label(&settings(&canonical_root)),
        labels_by_account: cfg
            .accounts
            .iter()
            .map(|a| (a.key.clone(), env::default_labels(&settings(&a.root))))
            .collect(),
        style,
    }
}

#[expect(clippy::too_many_arguments, reason = "the outcome record is assembled once, here")]
fn launch_end(
    state: LaunchState,
    resume_id: Option<String>,
    transfer: Option<sessions::cross_account::Transfer>,
    fork: bool,
    view: LaunchView,
    restored: Vec<String>,
    project_source: String,
    preselect: String,
    chosen_how: Option<String>,
) -> UiEnd {
    UiEnd::Launch(Box::new(UiOutcome {
        state,
        resume_id,
        transfer,
        fork,
        view,
        restored,
        project_source,
        preselect,
        chosen_how,
    }))
}

fn trace_path(ctx: &PathCtx) -> PathBuf {
    let dir = ctx.home.join(".claude").join("claude-auto-logs");
    if std::fs::create_dir_all(&dir).is_err() {
        // The trace then simply fails to open its file.
    }
    let d = sys::local_now().map_or_else(String::new, |t| format!("{:04}-{:02}-{:02}", t.year, t.month, t.day));
    dir.join(format!("input-{d}-{}.log", std::process::id()))
}

/// `Confirm-HeldSession`: Enter = go on, Esc/Ctrl+C = back; nothing held = go on.
pub(super) fn confirm_held(term: &mut dyn Term, holder: Option<&str>, style: Style) -> Result<bool, UiStop> {
    let Some(h) = holder else { return Ok(true) };
    let warning = sessions::held_warning(h);
    let mut dirty = true;
    let tm = term.logger().timings.clone();
    loop {
        let size = term.size();
        if dirty {
            let (w, h) = view::body_size(&tm, size);
            let lines = frame::held_session_frame(&warning, w, h, style.ascii);
            term.paint(&view::with_footer(&tm, lines, "held", size));
            dirty = false;
        }
        match term.wait(size)? {
            Event::Resize => dirty = true,
            Event::Key(k) if tm.is_on() && k.is(vk::F12) => {
                view::show_timings(term, &tm)?;
                dirty = true;
            }
            Event::Key(k) if k.is(vk::ENTER) => return Ok(true),
            Event::Key(k) if k.is(vk::ESCAPE) || k.is_ctrl(vk::C) => return Ok(false),
            Event::Key(_) | Event::Mouse(_) => {}
        }
    }
}
