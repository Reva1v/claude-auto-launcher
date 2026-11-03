use serde_json::json;

use super::*;
use crate::prefs::parse_prefs;
use crate::types::LimitRecord;

fn account(key: &str, canonical: bool, hidden: bool) -> Account {
    Account {
        key: key.to_owned(),
        root: format!(r"C:\h\.{key}"),
        label: format!("{key} label"),
        tint: if canonical { "Green" } else { "Magenta" }.to_owned(),
        hidden,
        canonical,
    }
}

fn accounts() -> Vec<Account> {
    vec![
        account("main", true, false),
        account("mamoru", false, false),
        account("old", false, true),
    ]
}

fn roster() -> Roster {
    Roster::new(&accounts(), true, Some("main"))
}

fn args(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

const FULL: &str = "--account main --project C:\\p --model opus1m --effort high --advisor off --permission auto --mode safe --action new";

fn request() -> RunRequest {
    RunRequest {
        account: "main".into(),
        project: std::env::temp_dir().to_string_lossy().into_owned(),
        model: "opus1m".into(),
        effort: "high".into(),
        advisor: "off".into(),
        permission: "auto".into(),
        mode: "safe".into(),
        action: "new".into(),
        resume_id: None,
        fork: false,
    }
}

fn limits_without_fable(account: &str) -> Limits {
    let record = LimitRecord {
        five_hour: Some(16),
        available_models: Some(vec!["opus".to_owned()]),
        ..LimitRecord::default()
    };
    [(account.to_owned(), record)].into()
}

fn resolve(req: &RunRequest) -> Result<LaunchState, String> {
    resolve_run(req, &roster(), &Limits::new(), &Prefs::default())
}

#[test]
fn a_full_command_line_becomes_a_request() {
    let req = parse_run_args(&args(&format!("{FULL} --resume-id abc --fork"))).unwrap();
    assert_eq!(req.account, "main");
    assert_eq!(req.project, r"C:\p");
    assert_eq!(req.model, "opus1m");
    assert_eq!(req.effort, "high");
    assert_eq!(req.advisor, "off");
    assert_eq!(req.permission, "auto");
    assert_eq!(req.mode, "safe");
    assert_eq!(req.action, "new");
    assert_eq!(req.resume_id.as_deref(), Some("abc"));
    assert!(req.fork);
}

#[test]
fn every_row_flag_is_required_so_a_dropped_one_cannot_launch_on_a_silent_default() {
    for flag in [
        "--account",
        "--project",
        "--model",
        "--effort",
        "--advisor",
        "--permission",
        "--mode",
        "--action",
    ] {
        let line: Vec<String> = args(FULL);
        let at = line.iter().position(|a| a == flag).unwrap();
        let without: Vec<String> = line
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != at && *i != at + 1)
            .map(|(_, a)| a.clone())
            .collect();
        let err = parse_run_args(&without).unwrap_err();
        assert!(err.contains(flag), "{flag}: {err}");
    }
}

#[test]
fn a_mistyped_flag_is_refused_rather_than_handed_to_claude() {
    let err = parse_run_args(&args(&format!("{FULL} --dangerously-skip-permissions"))).unwrap_err();
    assert!(err.contains("--dangerously-skip-permissions"), "{err}");
}

#[test]
fn a_value_that_looks_like_a_flag_is_refused_so_it_cannot_reach_claude_as_an_option() {
    let err = parse_run_args(&args(&format!("{FULL} --resume-id --dangerously-skip-permissions"))).unwrap_err();
    assert!(err.contains("--resume-id"), "{err}");
    assert!(
        parse_run_args(&args(&format!("{FULL} --resume-id"))).is_err(),
        "a flag at the end has no value"
    );
}

#[test]
fn a_repeated_flag_is_refused_because_either_reading_of_it_would_be_a_guess() {
    assert!(parse_run_args(&args(&format!("{FULL} --model haiku"))).is_err());
    assert!(parse_run_args(&args(&format!("{FULL} --fork --fork"))).is_err());
}

#[test]
fn an_unknown_account_stops_the_run_before_claude_starts_on_the_wrong_profile() {
    let mut req = request();
    req.account = "nobody".into();
    let err = resolve(&req).unwrap_err();
    assert!(err.contains("nobody"), "{err}");
}

#[test]
fn an_archived_account_is_off_the_screen_so_it_is_off_the_phone() {
    let mut req = request();
    req.account = "old".into();
    assert!(resolve(&req).is_err());
}

#[test]
fn the_project_action_is_refused_because_the_phone_already_chose_the_project() {
    let mut req = request();
    req.action = "project".into();
    let err = resolve(&req).unwrap_err();
    assert!(err.contains("--action"), "{err}");
}

#[test]
fn a_value_outside_its_row_names_the_flag() {
    for (flag, set) in [
        ("--model", (|r, v| r.model = v) as fn(&mut RunRequest, String)),
        ("--effort", |r, v| r.effort = v),
        ("--advisor", |r, v| r.advisor = v),
        ("--permission", |r, v| r.permission = v),
        ("--mode", |r, v| r.mode = v),
        ("--action", |r, v| r.action = v),
    ] {
        let mut req = request();
        set(&mut req, "nonsense".to_owned());
        let err = resolve(&req).unwrap_err();
        assert!(err.contains(flag), "{flag}: {err}");
    }
}

#[test]
fn a_family_the_account_lacks_is_refused_because_the_screen_would_not_offer_it() {
    let mut req = request();
    req.model = "fable".into();
    let lacking = limits_without_fable("main");
    let err = resolve_run(&req, &roster(), &lacking, &Prefs::default()).unwrap_err();
    assert!(err.contains("--model"), "{err}");
    req.model = "opus1m".into();
    req.advisor = "fable".into();
    let err = resolve_run(&req, &roster(), &lacking, &Prefs::default()).unwrap_err();
    assert!(err.contains("--advisor"), "{err}");
}

#[test]
fn another_accounts_availability_does_not_hide_a_family_here() {
    let mut req = request();
    req.model = "fable".into();
    let state = resolve_run(&req, &roster(), &limits_without_fable("mamoru"), &Prefs::default()).unwrap();
    assert_eq!(state.model, "fable");
}

#[test]
fn no_availability_data_hides_nothing_as_on_the_screen() {
    let mut req = request();
    req.model = "fable".into();
    assert_eq!(resolve(&req).unwrap().model, "fable");
}

#[test]
fn a_missing_project_directory_is_refused_because_claude_would_open_in_the_wrong_folder() {
    let mut req = request();
    req.project = std::env::temp_dir()
        .join("claude-auto-no-such-dir-024")
        .to_string_lossy()
        .into_owned();
    let err = resolve(&req).unwrap_err();
    assert!(err.contains("--project"), "{err}");
}

#[test]
fn resume_without_an_id_is_refused_because_launch_args_would_drop_the_flag_and_start_a_new_session() {
    let mut req = request();
    req.action = "resume".into();
    let err = resolve(&req).unwrap_err();
    assert!(err.contains("--resume-id"), "{err}");
    req.action = "RESUME".into();
    assert!(resolve(&req).is_err(), "the row compares case-insensitively, the check too");
    req.resume_id = Some("abc".into());
    assert_eq!(resolve(&req).unwrap().action, "resume");
}

#[test]
fn values_take_the_rows_own_casing_because_set_profile_and_launch_args_compare_exactly() {
    let mut req = request();
    req.account = "MAIN".into();
    req.permission = "ACCEPTEDITS".into();
    let state = resolve(&req).unwrap();
    assert_eq!(state.account, "main");
    assert_eq!(state.permission, "acceptEdits");
}

#[test]
fn the_state_carries_the_request_and_the_remembered_remote() {
    let prefs = parse_prefs(r#"{"Version":2,"Account":"main","Profiles":{"main":{"Remote":"off","Model":"haiku"}}}"#);
    let req = request();
    let state = resolve_run(&req, &roster(), &Limits::new(), &prefs).unwrap();
    assert_eq!(state.model, "opus1m", "the request wins over the remembered row");
    assert_eq!(state.effort, "high");
    assert_eq!(state.advisor, "off");
    assert_eq!(state.permission, "auto");
    assert_eq!(state.mode, "safe");
    assert_eq!(state.action, "new");
    assert_eq!(state.project, req.project);
    assert_eq!(state.remote, "off", "the saved row, not a phone's opinion of it");
}

#[test]
fn a_saved_remote_off_never_turns_the_bridge_off_for_a_phone_launch() {
    assert!(
        remote_wanted(true, "off", true),
        "the phone reached this launcher through that bridge"
    );
    assert_eq!(
        bridge_step(true, remote_wanted(true, "off", true), LaunchSource::Phone),
        BridgeStep::Ensure
    );
    assert_eq!(
        bridge_step(true, false, LaunchSource::Phone),
        BridgeStep::Ensure,
        "not even if the row said off"
    );
}

#[test]
fn the_screen_keeps_its_own_remote_switch() {
    assert!(!remote_wanted(true, "off", false));
    assert!(remote_wanted(true, "on", false));
    assert_eq!(bridge_step(true, false, LaunchSource::ScreenOrBare), BridgeStep::Stop);
    assert_eq!(bridge_step(true, true, LaunchSource::ScreenOrBare), BridgeStep::Ensure);
}

#[test]
fn a_launch_with_arguments_and_a_disabled_feature_leave_the_bridge_alone() {
    assert_eq!(bridge_step(true, true, LaunchSource::WithArguments), BridgeStep::Leave);
    assert_eq!(bridge_step(false, false, LaunchSource::ScreenOrBare), BridgeStep::Leave);
    assert_eq!(bridge_step(false, false, LaunchSource::Phone), BridgeStep::Leave);
    assert!(!remote_wanted(false, "on", true), "no remote feature: nothing to want");
}

struct Options {
    limits: Limits,
    labels: BTreeMap<String, DefaultLabels>,
    prefs: Prefs,
}

impl Options {
    fn new() -> Self {
        Self {
            limits: Limits::new(),
            labels: BTreeMap::new(),
            prefs: Prefs::default(),
        }
    }

    fn json(&self) -> Value {
        options_json(&roster(), &accounts(), &self.limits, &self.labels, &self.prefs, Some("2.1.317"))
    }

    fn main_account(&self) -> Value {
        self.json()["accounts"][0].clone()
    }
}

#[test]
fn the_rows_follow_the_screen_without_account_remote_and_the_project_action() {
    let v = Options::new().json();
    let names: Vec<&str> = v["rows"].as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["action", "model", "effort", "advisor", "permission", "mode"]);
    let actions: Vec<&str> = v["rows"][0]["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["value"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["new", "continue", "resume", "worktree"]);
}

#[test]
fn every_value_on_a_row_carries_the_label_the_screen_draws() {
    let v = Options::new().json();
    let model = &v["rows"][1];
    assert_eq!(model["label"], "model");
    assert_eq!(model["values"][0], json!({"value": "default", "label": "default"}));
    assert_eq!(model["values"][2], json!({"value": "opus1m", "label": "Opus 5.5[1M]"}));
    let permission = &v["rows"][4];
    assert_eq!(permission["values"][3], json!({"value": "acceptEdits", "label": "acceptEdits"}));
}

#[test]
fn the_head_names_the_launcher_and_the_cli_version() {
    let v = Options::new().json();
    assert_eq!(v["launcher"], "claude-auto");
    assert_eq!(v["version"], "2.1.317");
    let none = options_json(&roster(), &accounts(), &Limits::new(), &BTreeMap::new(), &Prefs::default(), None);
    assert_eq!(none["version"], Value::Null, "an unknown version is said, not invented");
}

#[test]
fn accounts_are_those_of_the_screen_with_the_default_marked() {
    let v = Options::new().json();
    let list = v["accounts"].as_array().unwrap();
    assert_eq!(list.len(), 2, "the archived account is not offered");
    let main = &list[0];
    assert_eq!(main["key"], "main");
    assert_eq!(main["label"], "main label");
    assert_eq!(main["tint"], "Green");
    assert_eq!(main["configDir"], r"C:\h\.main");
    assert_eq!(main["default"], true);
    assert_eq!(list[1]["default"], false);
}

#[test]
fn without_data_an_account_has_null_and_empty_objects_rather_than_guesses() {
    let main = Options::new().main_account();
    assert_eq!(main["limitPct"], Value::Null);
    assert_eq!(main["hidden"], json!({}));
    assert_eq!(main["defaultLabels"], json!({}));
    assert_eq!(main["prefs"], json!({}));
}

#[test]
fn the_limit_is_the_five_hour_percent_the_account_tab_shows() {
    let mut o = Options::new();
    o.limits = limits_without_fable("main");
    assert_eq!(o.main_account()["limitPct"], 16);
}

#[test]
fn a_family_the_account_lacks_is_listed_as_hidden_per_row() {
    let mut o = Options::new();
    o.limits = limits_without_fable("main");
    let v = o.json();
    assert_eq!(v["accounts"][0]["hidden"], json!({"model": ["fable"], "advisor": ["fable"]}));
    assert_eq!(
        v["accounts"][1]["hidden"],
        json!({}),
        "the other account answered nothing, so nothing is hidden"
    );
}

fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, DefaultLabels> {
    let inner: DefaultLabels = pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    [("main".to_owned(), inner)].into()
}

#[test]
fn default_labels_say_what_default_resolves_to_and_leave_out_the_uninformative() {
    let mut o = Options::new();
    o.labels = labels(&[
        ("Model", "Opus 5.5"),
        ("Effort", "high"),
        ("Advisor", "default"),
        ("Permission", "default"),
    ]);
    assert_eq!(
        o.main_account()["defaultLabels"],
        json!({"model": "default (Opus 5.5)", "effort": "default (high)"})
    );
}

#[test]
fn a_default_of_a_lacking_family_is_labelled_by_the_pin_the_launch_will_pass() {
    let mut o = Options::new();
    o.limits = limits_without_fable("main");
    o.labels = labels(&[("Model", "Fable 5.1"), ("Advisor", "fable")]);
    assert_eq!(
        o.main_account()["defaultLabels"],
        json!({"model": "default (Opus 5.5[1M])", "advisor": "default (opus)"})
    );
}

const SAVED: &str = r#"{"Version":2,"Account":"main","Profiles":{"main":{"Model":"fable","Effort":"medium","Advisor":"OFF","Remote":"off","Project":"C:\\p"}}}"#;

#[test]
fn prefs_are_the_rows_the_screen_would_restore_and_never_the_remote() {
    let mut o = Options::new();
    o.prefs = parse_prefs(SAVED);
    assert_eq!(
        o.main_account()["prefs"],
        json!({"model": "fable", "effort": "medium", "advisor": "off", "permission": "default"})
    );
}

#[test]
fn a_remembered_family_the_account_lost_goes_back_to_default_as_on_the_screen() {
    let mut o = Options::new();
    o.prefs = parse_prefs(SAVED);
    o.limits = limits_without_fable("main");
    assert_eq!(o.main_account()["prefs"]["model"], "default");
}

#[test]
fn a_profile_holding_only_the_remote_has_no_prefs_to_offer() {
    let mut o = Options::new();
    o.prefs = parse_prefs(r#"{"Version":2,"Profiles":{"main":{"Remote":"off"}}}"#);
    assert_eq!(o.main_account()["prefs"], json!({}));
}
