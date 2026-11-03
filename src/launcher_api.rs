//! (local) The launcher without its screen, for a launch started from the phone through
//! claude-tg-bridge: `--launcher-options` says what the screen would offer, `--launcher-run` takes
//! those choices and joins the launch after the screen. Pure: the reads and the launch are in
//! `app/headless.rs`.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::config::Account;
use crate::launch::{LaunchState, OptionText, Roster, RowDef, RowName, availability_snap, launch_mode, visible_row_values};
use crate::prefs::{self, Prefs};
use crate::types::{DefaultLabels, Limits};

pub const OPTIONS_FLAG: &str = "--launcher-options";
pub const RUN_FLAG: &str = "--launcher-run";

/// The phone has chosen its project already, and the screen's `project` value opens a screen.
const ACTION_NOT_FOR_PHONE: &str = "project";

const VALUE_FLAGS: [&str; 9] = [
    "--account",
    "--project",
    "--model",
    "--effort",
    "--advisor",
    "--permission",
    "--mode",
    "--action",
    "--resume-id",
];

/// What `--launcher-run` was asked, before any of it is checked against the rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunRequest {
    pub account: String,
    pub project: String,
    pub model: String,
    pub effort: String,
    pub advisor: String,
    pub permission: String,
    pub mode: String,
    pub action: String,
    pub resume_id: Option<String>,
    pub fork: bool,
}

/// The arguments after `--launcher-run`. Strict on purpose: the bridge builds this line, so anything
/// odd is a bug or an attempt, and a launch on a guessed reading is worse than none. A value starting
/// with `-` is refused because the id ends up as an argument of `claude`.
pub fn parse_run_args(args: &[String]) -> Result<RunRequest, String> {
    let mut values: BTreeMap<&str, String> = BTreeMap::new();
    let mut fork = false;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        if flag == "--fork" {
            if fork {
                return Err("--fork: given twice".to_owned());
            }
            fork = true;
            continue;
        }
        let Some(name) = VALUE_FLAGS.iter().find(|f| **f == flag) else {
            return Err(format!("unknown argument '{flag}'"));
        };
        let value = it
            .next()
            .filter(|v| !v.is_empty() && !v.starts_with('-'))
            .ok_or_else(|| format!("{name}: needs a value"))?;
        if values.insert(name, value.clone()).is_some() {
            return Err(format!("{name}: given twice"));
        }
    }
    let mut take = |name: &str| values.remove(name).ok_or_else(|| format!("{name}: required"));
    Ok(RunRequest {
        account: take("--account")?,
        project: take("--project")?,
        model: take("--model")?,
        effort: take("--effort")?,
        advisor: take("--advisor")?,
        permission: take("--permission")?,
        mode: take("--mode")?,
        action: take("--action")?,
        resume_id: values.remove("--resume-id"),
        fork,
    })
}

static NO_LABELS: std::sync::LazyLock<DefaultLabels> = std::sync::LazyLock::new(DefaultLabels::new);

/// What the row offers this account, a twin folded into `default` included: the screen stores the
/// twin when it lands there, so the twin is a value of the row and the client gets `defaultLabels`.
fn offered(roster: &Roster, row: &RowDef, available: Option<&[String]>) -> Vec<String> {
    visible_row_values(roster, row, &OptionText::plain(available, &NO_LABELS))
        .into_iter()
        .filter(|v| row.name != RowName::Action || !v.eq_ignore_ascii_case(ACTION_NOT_FOR_PHONE))
        .collect()
}

fn available<'a>(limits: &'a Limits, account: &str) -> Option<&'a [String]> {
    limits.get(account).and_then(|l| l.available_models.as_deref())
}

/// Checks the request against what the screen would have allowed on this account and builds the
/// state the launch reads. The rows that are not asked for (`remote`) stay as remembered.
pub fn resolve_run(req: &RunRequest, roster: &Roster, limits: &Limits, prefs_file: &Prefs) -> Result<LaunchState, String> {
    let account = roster
        .row(RowName::Account)
        .and_then(|r| r.canonical(&req.account))
        .ok_or_else(|| format!("--account: unknown account '{}'", req.account))?;
    let have = available(limits, account);
    let mut state = prefs::account_state(prefs_file, account, roster);
    state.restored.clear();
    for (name, flag, value) in [
        (RowName::Model, "--model", &req.model),
        (RowName::Effort, "--effort", &req.effort),
        (RowName::Advisor, "--advisor", &req.advisor),
        (RowName::Permission, "--permission", &req.permission),
        (RowName::Mode, "--mode", &req.mode),
        (RowName::Action, "--action", &req.action),
    ] {
        let values = roster.row(name).map_or_else(Vec::new, |r| offered(roster, r, have));
        let Some(canon) = values.iter().find(|v| v.eq_ignore_ascii_case(value)) else {
            return Err(format!("{flag}: '{value}' is not one of {}", values.join(", ")));
        };
        state.set(name, canon);
    }
    if launch_mode(&state.action).is_some_and(|m| m.picks_session) && req.resume_id.is_none() {
        return Err("--resume-id: required with --action resume".to_owned());
    }
    if !Path::new(&req.project).is_dir() {
        return Err(format!("--project: no such directory '{}'", req.project));
    }
    req.project.clone_into(&mut state.project);
    Ok(state)
}

fn row_json(roster: &Roster, row: &RowDef) -> Value {
    let text = OptionText::plain(None, &NO_LABELS);
    let values: Vec<Value> = offered(roster, row, None)
        .iter()
        .map(|v| json!({ "value": v, "label": text.text(roster, row, v) }))
        .collect();
    json!({ "name": row.label, "label": row.label, "values": values })
}

/// The rows a phone can set, in screen order: the account is chosen separately, and the bridge row is
/// managed on the PC only.
fn phone_rows(roster: &Roster) -> impl Iterator<Item = &RowDef> {
    roster.rows.iter().filter(|r| !matches!(r.name, RowName::Account | RowName::Remote))
}

/// Row -> the values of the row this account does not offer.
fn hidden_json(roster: &Roster, have: Option<&[String]>) -> Map<String, Value> {
    phone_rows(roster)
        .filter_map(|row| {
            let shown = offered(roster, row, have);
            let mut gone = offered(roster, row, None);
            gone.retain(|v| !shown.contains(v));
            (!gone.is_empty()).then(|| (row.label.to_owned(), json!(gone)))
        })
        .collect()
}

/// Row -> `default (what it resolves to)`, through the same text the screen draws, the pin of a
/// lacking family included. A default that resolves to nothing says nothing.
fn default_labels_json(roster: &Roster, have: Option<&[String]>, labels: Option<&DefaultLabels>) -> Map<String, Value> {
    let labels = labels.unwrap_or(&NO_LABELS);
    let text = OptionText::plain(have, labels);
    phone_rows(roster)
        .filter(|row| labels.contains_key(row.name.as_str()))
        .filter_map(|row| {
            let resolved = text.text(roster, row, "default");
            (!resolved.eq_ignore_ascii_case("default")).then(|| (row.label.to_owned(), json!(format!("default ({resolved})"))))
        })
        .collect()
}

/// The remembered rows as the screen would restore them. A profile with nothing the phone can use is
/// no data; `remote` is not among them, it is the PC's.
fn prefs_json(roster: &Roster, prefs_file: &Prefs, account: &str, have: Option<&[String]>) -> Map<String, Value> {
    let mut state = prefs::account_state(prefs_file, account, roster);
    availability_snap(&mut state, have);
    let rows = [RowName::Model, RowName::Effort, RowName::Advisor, RowName::Permission];
    if !rows.iter().any(|r| state.is_restored(*r)) {
        return Map::new();
    }
    rows.iter()
        .filter_map(|r| roster.row(*r).map(|row| (row.label.to_owned(), json!(state.get(*r)))))
        .collect()
}

/// The one line `--launcher-options` prints. `labels`: account -> its resolved defaults.
pub fn options_json(
    roster: &Roster,
    accounts: &[Account],
    limits: &Limits,
    labels: &BTreeMap<String, DefaultLabels>,
    prefs_file: &Prefs,
    version: Option<&str>,
) -> Value {
    let on_screen = roster.row(RowName::Account);
    let listed: Vec<Value> = accounts
        .iter()
        .filter(|a| on_screen.is_some_and(|r| r.values.contains(&a.key)))
        .map(|a| {
            let have = available(limits, &a.key);
            json!({
                "key": a.key,
                "label": a.label,
                "tint": a.tint,
                "configDir": a.root,
                "default": a.key == roster.default_account(),
                "limitPct": limits.get(&a.key).and_then(|l| l.five_hour),
                "hidden": hidden_json(roster, have),
                "defaultLabels": default_labels_json(roster, have, labels.get(&a.key)),
                "prefs": prefs_json(roster, prefs_file, &a.key, have),
            })
        })
        .collect();
    let rows: Vec<Value> = phone_rows(roster).map(|r| row_json(roster, r)).collect();
    json!({ "launcher": "claude-auto", "version": version, "rows": rows, "accounts": listed })
}

/// What the launch does to the bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeStep {
    Leave,
    Ensure,
    Stop,
}

/// Does this launch want the bridge up. A phone launch came through that bridge: a remembered `off`
/// obeyed here would cut the phone off from the session it just started.
pub fn remote_wanted(feature_on: bool, saved_row: &str, phone: bool) -> bool {
    feature_on && (phone || saved_row != "off")
}

/// Is this launch's choice remembered for the next one. The phone's pick is for that one session, by
/// the owner's decision: saved, the next launch on the PC would quietly start on it.
pub const fn saves_choice(preview: bool, phone: bool) -> bool {
    !preview && !phone
}

/// Where the launch came from, as far as the bridge cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchSource {
    ScreenOrBare,
    /// `-p`, a script: leaves the bridge alone.
    WithArguments,
    Phone,
}

/// A phone launch only ever raises the bridge, whatever else was decided.
pub fn bridge_step(feature_on: bool, remote_on: bool, source: LaunchSource) -> BridgeStep {
    if !feature_on || source == LaunchSource::WithArguments {
        BridgeStep::Leave
    } else if source == LaunchSource::Phone || remote_on {
        BridgeStep::Ensure
    } else {
        BridgeStep::Stop
    }
}

#[cfg(test)]
mod tests;
