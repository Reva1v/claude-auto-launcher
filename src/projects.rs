//! Projects and the project screen (`Projects.ps1`, `Invoke-ProjectScreen`, `Get-ProjectFrame`):
//! the registry of directories Claude Code has sessions in, the start selection, and the screen
//! that picks where a session runs and what it does there.

mod paths;
mod registry;
mod screen;
mod select;
mod view;
mod win;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::frame::Style;
use crate::launch::LaunchState;
use crate::ui::{Term, UiStop};

pub use paths::project_key;
pub(crate) use registry::path_from_transcript;

/// One project row (`Get-ProjectRegistry`): one real directory, every slug folder naming it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// The directory's leaf name (`C:` for a drive root).
    pub name: String,
    /// The cwd the newest transcript recorded.
    pub path: String,
    /// The slug of the newest activity.
    pub slug: String,
    /// Every slug of this directory: what the session picker scopes on.
    pub slugs: Vec<String>,
    /// The worktree name a `--claude-worktrees-` slug carries.
    pub worktree: Option<String>,
    /// Unix milliseconds of the newest interactive transcript (else the newest).
    pub last_activity_ms: Option<i64>,
    /// A scratch directory, or a folder only headless runs used that is no repository root: known,
    /// listed only by a filter or as the cwd.
    pub hidden: bool,
}

/// What the project screen returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectChoice {
    pub path: String,
    /// A launch-mode name (`launch::LAUNCH_MODES`).
    pub action: String,
    pub slug: String,
    pub slugs: Vec<String>,
    /// enter | hotkey | mouse.
    pub how: String,
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// `Get-ProjectRegistry`. The projects root is `CLAUDE_AUTO_PROJECTS_ROOT`, else
/// `~\.claude\projects`; the cache is `cache_path` (preview: its own per-PID file), else
/// `claude-auto-projects.json` beside the root. `profile_roots`: every account root a job tmp
/// directory can sit under. Never fails: an unreadable root is an empty list.
pub fn project_registry(cache_path: Option<&Path>, profile_roots: &[String]) -> Vec<Project> {
    let home = home();
    let root = std::env::var_os("CLAUDE_AUTO_PROJECTS_ROOT")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".claude").join("projects"), PathBuf::from);
    let cache = cache_path.map_or_else(
        || root.parent().unwrap_or(&root).join("claude-auto-projects.json"),
        Path::to_path_buf,
    );
    let temp = paths::temp_roots(&paths::default_temp_candidates(), &home.to_string_lossy());
    registry::build_registry(&registry::RegistryInput {
        projects_root: &root,
        cache_path: &cache,
        temp_roots: &temp,
        profile_roots,
    })
}

/// (local) `Get-AllAccountsProjectRegistry`: every project Claude was opened in, from EVERY account's
/// `<root>\projects`, one row per real directory: every slug (the picker scopes on them and reads
/// both stores), the newest activity, and hidden only when no account shows it. `preview` keeps each
/// registry's cache in the temp folder. `CLAUDE_AUTO_PROJECTS_ROOT` still names one root.
pub fn all_accounts_registry(account_roots: &[String], profile_roots: &[String], preview: bool) -> Vec<Project> {
    if std::env::var_os("CLAUDE_AUTO_PROJECTS_ROOT").is_some_and(|v| !v.is_empty()) {
        let cache = preview.then(|| std::env::temp_dir().join(format!("claude-auto-projects-preview-{}.json", std::process::id())));
        return project_registry(cache.as_deref(), profile_roots);
    }
    let temp = paths::temp_roots(&paths::default_temp_candidates(), &home().to_string_lossy());
    let lists = account_roots.iter().enumerate().map(|(i, root)| {
        let projects_root = Path::new(root).join("projects");
        let cache_path = if preview {
            std::env::temp_dir().join(format!("claude-auto-projects-preview-{}-{i}.json", std::process::id()))
        } else {
            Path::new(root).join("claude-auto-projects.json")
        };
        registry::build_registry(&registry::RegistryInput {
            projects_root: &projects_root,
            cache_path: &cache_path,
            temp_roots: &temp,
            profile_roots,
        })
    });
    merge_registries(lists)
}

fn merge_registries(lists: impl Iterator<Item = Vec<Project>>) -> Vec<Project> {
    let mut merged: Vec<(String, Project)> = Vec::new();
    for p in lists.flatten() {
        let key = project_key(&p.path);
        let Some((_, have)) = merged.iter_mut().find(|(k, _)| *k == key) else {
            merged.push((key, p));
            continue;
        };
        let mut slugs = std::mem::take(&mut have.slugs);
        let hidden = have.hidden && p.hidden;
        let newer = p.last_activity_ms > have.last_activity_ms;
        let (first, second) = if newer { (&p.slugs, &slugs) } else { (&slugs, &p.slugs) };
        let mut all: Vec<String> = first.iter().chain(second).cloned().collect();
        let mut seen = std::collections::HashSet::new();
        all.retain(|s| seen.insert(s.to_lowercase()));
        slugs = all;
        if newer {
            *have = p;
        }
        have.slugs = slugs;
        have.hidden = hidden;
    }
    let mut out: Vec<Project> = merged.into_iter().map(|(_, p)| p).collect();
    out.sort_by_key(|p| std::cmp::Reverse(p.last_activity_ms));
    out
}

/// (local) Every slug of the directory a path names (the launch directory's picker scope).
pub fn slugs_for(projects: &[Project], path: &Path) -> Vec<String> {
    select::slugs_for_path(projects, &path.to_string_lossy())
}

/// The profile roots the launcher hands `project_registry` (`Get-ProjectProfileRoots` plus the
/// roster, claude-auto.ps1): `~\.claude`, the `CLAUDE_CONFIG_DIR` this process started with, then
/// every account root.
pub fn launcher_profile_roots(home: &Path, config_dir: Option<&str>, account_roots: &[String]) -> Vec<String> {
    let mut roots = vec![home.join(".claude").to_string_lossy().into_owned()];
    roots.extend(config_dir.filter(|d| !d.is_empty()).map(str::to_owned));
    roots.extend(account_roots.iter().filter(|r| !r.is_empty()).cloned());
    roots
}

/// The launch context the start rule is re-applied with on a tab switch (`LaunchState::start`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartContext {
    cwd: String,
    projects: Vec<Project>,
}

fn apply_start(state: &mut LaunchState, cwd: &str, projects: &[Project]) -> &'static str {
    let (path, source) = select::resolve_start(cwd, &state.project, projects);
    state.project = path.unwrap_or_default();
    state.project_slug.clear();
    state.project_slugs.clear();
    if !state.project.is_empty() {
        let key = project_key(&state.project);
        let hits: Vec<&Project> = projects.iter().filter(|p| project_key(&p.path) == key).collect();
        if let Some(first) = hits.first() {
            state.project_slug.clone_from(&first.slug);
            state.project_slugs = hits
                .iter()
                .flat_map(|p| {
                    if p.slugs.is_empty() {
                        vec![p.slug.clone()]
                    } else {
                        p.slugs.clone()
                    }
                })
                .collect();
        }
    }
    source.as_str()
}

/// `Set-LaunchStartProject`: the cwd when it is a known project or holds a `.git`, else the
/// remembered project (`state.project`) when it is still there, else none. Sets the project, its
/// slug and every slug of its directory; records the context for `update_start_selection`.
/// Returns the source: cwd | remembered | none.
pub fn set_launch_start_project(state: &mut LaunchState, cwd: &Path, projects: &[Project]) -> String {
    let cwd = cwd.to_string_lossy().into_owned();
    let source = apply_start(state, &cwd, projects);
    state.start = Some(StartContext {
        cwd,
        projects: projects.to_vec(),
    });
    source.into()
}

/// `Update-LaunchStartSelection`: re-decides the start selection after a tab switch replaced the
/// remembered project. A no-op when no start selection ran.
pub fn update_start_selection(state: &mut LaunchState) {
    if let Some(ctx) = state.start.take() {
        apply_start(state, &ctx.cwd, &ctx.projects);
        state.start = Some(ctx);
    }
}

/// `Remove-StalePreviewProjectsCache`: the per-PID preview caches a killed run left, older than a
/// day, by their exact name only.
pub fn remove_stale_preview_cache(dir: &Path) {
    registry::remove_stale_preview_cache_at(dir, SystemTime::now());
}

/// (local) This run's per-PID preview caches, one per account. Best effort: the next preview sweeps
/// what a failure leaves.
pub fn remove_own_preview_caches(dir: &Path) {
    let prefix = format!("claude-auto-projects-preview-{}", std::process::id());
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if e.file_name().to_string_lossy().starts_with(&prefix) && std::fs::remove_file(e.path()).is_err() {
            // Swept by age on a later preview.
        }
    }
}

/// `Invoke-ProjectScreen`: where the session runs and what it does there. `None` = Escape, back to
/// the launch screen. The cursor opens on the current directory; the action field on `action`
/// (local: the launch screen's own row, so Enter here runs what was chosen one screen up).
pub fn run_project_screen(
    term: &mut dyn Term,
    projects: &[Project],
    cwd: &Path,
    action: &str,
    style: Style,
) -> Result<Option<ProjectChoice>, UiStop> {
    let clock = || registry::unix_ms(SystemTime::now());
    screen::run_screen(term, projects, cwd, action, style, &clock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::registry::tests::Tree;
    use crate::projects::select::tests::proj;

    fn state() -> LaunchState {
        LaunchState::fresh("work")
    }

    #[test]
    fn set_launch_start_project_applies_path_and_slugs() {
        let ps = vec![proj("SLS-A", r"C:\w\alpha", "alpha")];
        let mut s = state();
        s.project_slug = "stale".into();
        assert_eq!(set_launch_start_project(&mut s, Path::new(r"C:\w\alpha"), &ps), "cwd");
        assert_eq!((s.project.as_str(), s.project_slug.as_str()), (r"C:\w\alpha", "SLS-A"));
        let mut n = state();
        assert_eq!(
            set_launch_start_project(&mut n, Path::new(r"C:\Users\someone\Desktop"), &ps),
            "none"
        );
        assert_eq!((n.project.as_str(), n.project_slug.as_str()), ("", ""));
        let t = Tree::new("startgit");
        let git = t.dir(r"repo\.git");
        let repo = Path::new(&git).parent().unwrap().to_path_buf();
        let mut g = state();
        assert_eq!(set_launch_start_project(&mut g, &repo, &ps), "cwd");
        assert_eq!((g.project.as_str(), g.project_slug.as_str()), (repo.to_str().unwrap(), ""));
    }

    #[test]
    fn registries_merge_per_directory_with_every_slug_and_the_newest_activity() {
        let row = |slug: &str, path: &str, ms: i64, hidden: bool| Project {
            slugs: vec![slug.to_owned()],
            last_activity_ms: Some(ms),
            hidden,
            ..proj(slug, path, "p")
        };
        let main = vec![row("M", r"C:\w\both", 100, true), row("OLD", r"C:\w\main-only", 50, false)];
        let mamoru = vec![row("R", r"c:/w/BOTH/", 200, false), row("NEW", r"C:\w\mamoru-only", 300, false)];
        let got = merge_registries([main, mamoru].into_iter());
        assert_eq!(
            got.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
            ["NEW", "R", "OLD"],
            "one row per directory, newest first, the newer account's slug on top"
        );
        assert_eq!(got[1].slugs, ["R", "M"]);
        assert_eq!(got[1].last_activity_ms, Some(200));
        assert!(!got[1].hidden, "hidden only when no account shows it");
    }

    #[test]
    fn launcher_profile_roots_add_the_default_root_and_config_dir() {
        let roster = vec![r"C:\h\.claude-acct2".to_owned(), String::new()];
        let roots = launcher_profile_roots(Path::new(r"C:\h"), Some(r"D:\cfg\.claude-x"), &roster);
        assert_eq!(roots, [r"C:\h\.claude", r"D:\cfg\.claude-x", r"C:\h\.claude-acct2"]);
        let refs: Vec<&str> = roots.iter().map(String::as_str).collect();
        assert!(
            paths::is_scratch_path(r"D:\cfg\.claude-x\jobs\76c3aa80\tmp\w1", &[], &refs),
            "a job tmp under CLAUDE_CONFIG_DIR is scratch"
        );
        assert_eq!(
            launcher_profile_roots(Path::new(r"C:\h"), Some(""), &[]),
            [r"C:\h\.claude"],
            "an empty CLAUDE_CONFIG_DIR adds nothing"
        );
    }

    #[test]
    fn the_start_state_carries_every_slug_of_the_directory() {
        let mut p = proj("C--tmp-Shared", r"C:\tmp\Shared", "Shared");
        p.slugs = vec!["C--tmp-Shared".into(), "C--tmp-Shared-alt".into()];
        let mut s = state();
        set_launch_start_project(&mut s, Path::new(r"C:\tmp\Shared"), &[p]);
        assert_eq!(s.project_slugs, ["C--tmp-Shared", "C--tmp-Shared-alt"]);
    }

    #[test]
    fn a_tab_switch_reapplies_the_cwd_rule() {
        let ps = vec![proj("A", r"C:\w\alpha", "alpha")];
        let t = Tree::new("tabswitch");
        let remembered = t.dir("remembered");
        let mut s = state();
        update_start_selection(&mut s);
        assert_eq!(s.project, "", "no recorded context: a no-op");
        set_launch_start_project(&mut s, Path::new(r"C:\w\alpha"), &ps);
        s.project.clone_from(&remembered);
        update_start_selection(&mut s);
        assert_eq!(
            s.project, r"C:\w\alpha",
            "the cwd outranks the arriving account's remembered project"
        );
        let mut u = state();
        set_launch_start_project(&mut u, Path::new(r"C:\Users\someone\Desktop"), &ps);
        u.project.clone_from(&remembered);
        update_start_selection(&mut u);
        assert_eq!(u.project, remembered, "a neutral cwd takes the arriving account's project");
    }
}
