//! End-to-end through the built binary: preview frame dump, argv pass-through, the child marker,
//! `--mcp-config` placement, and `--launcher-version`. Every run gets its own fake home, so
//! nothing touches the real `~/.claude`.
#![expect(clippy::unwrap_used, reason = "test helpers fail the test on a broken fixture")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const EXE: &str = env!("CARGO_BIN_EXE_claude-auto");

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("claude-auto-rs-it-{name}-{}", std::process::id()));
    if d.exists() {
        std::fs::remove_dir_all(&d).unwrap();
    }
    std::fs::create_dir_all(d.join("home").join(".claude")).unwrap();
    std::fs::create_dir_all(d.join("bin")).unwrap();
    d
}

/// A fake `claude` that prints its argv and the launch marker, then exits 7.
fn fake_claude(dir: &Path) {
    std::fs::write(
        dir.join("bin").join("claude.cmd"),
        "@echo ARGV %*\r\n@echo MARK [%CLAUDE_AUTO_LAUNCHED%]\r\n@exit /b 7\r\n",
    )
    .unwrap();
}

fn launcher(dir: &Path) -> Command {
    let mut c = Command::new(EXE);
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    c.env("USERPROFILE", dir.join("home"))
        .env("PATH", format!(r"{};{system}\System32", dir.join("bin").display()))
        .env("PATHEXT", ".COM;.EXE;.BAT;.CMD")
        .env("CLAUDE_AUTO_CONFIG", dir.join("home").join("none.json"))
        .env("CLAUDE_AUTO_PREFS", dir.join("prefs.json"))
        .env("NO_COLOR", "1")
        .env("CLAUDE_NO_ROAM", "1")
        .env_remove("CLAUDE_AUTO_PREVIEW")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CLAUDE_AUTO_LAUNCHED")
        .env_remove("CLAUDE_AUTO_DEBUG")
        .current_dir(dir);
    c
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

#[test]
fn launcher_version_alone_prints_and_exits_zero() {
    let d = scratch("version");
    let o = launcher(&d).arg("--launcher-version").output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(text(&o).trim_end(), format!("claude-auto {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn arguments_pass_through_verbatim_with_exit_code_and_marker() {
    let d = scratch("pass");
    fake_claude(&d);
    std::fs::write(d.join("home").join(".claude").join("mcp-shared.json"), "{}").unwrap();
    let o = launcher(&d)
        .args(["--resume", "abc", "-p", "x y", "--launcher-version"])
        .output()
        .unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(7), "the child's exit code: {out}");
    let mcp = d.join("home").join(".claude").join("mcp-shared.json");
    // Rust quotes a batch-file argument that holds `~`, so a short temp path (RUNNER~1) arrives quoted.
    let head = "ARGV --resume abc -p \"x y\" --launcher-version --mcp-config ";
    let (plain, quoted) = (format!("{head}{}", mcp.display()), format!("{head}\"{}\"", mcp.display()));
    assert!(out.contains(&plain) || out.contains(&quoted), "{out}");
    assert!(out.contains("MARK [1]"), "CLAUDE_AUTO_LAUNCHED=1 for the child: {out}");
    assert!(out.contains("-> work account"), "{out}");
    std::fs::remove_dir_all(&d).unwrap();
}

fn timing_records(d: &Path) -> Vec<serde_json::Value> {
    let logs = d.join("home").join(".claude").join("launcher-logs");
    let Ok(entries) = std::fs::read_dir(&logs) else { return Vec::new() };
    entries
        .flatten()
        .flat_map(|e| {
            std::fs::read_to_string(e.path())
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
        .filter(|v| v["stage"] == "timing")
        .collect()
}

#[test]
fn debug_off_logs_no_timing_records_and_debug_on_logs_spans() {
    let d = scratch("timing-log");
    fake_claude(&d);
    let o = launcher(&d).args(["-p", "x"]).output().unwrap();
    assert_eq!(o.status.code(), Some(7), "{}", text(&o));
    assert_eq!(timing_records(&d), Vec::<serde_json::Value>::new());
    let o = launcher(&d).env("CLAUDE_AUTO_DEBUG", "1").args(["-p", "x"]).output().unwrap();
    assert_eq!(o.status.code(), Some(7), "{}", text(&o));
    let recs = timing_records(&d);
    let names: Vec<&str> = recs.iter().filter_map(|r| r["name"].as_str()).collect();
    for n in ["config", "process", "spawn"] {
        assert!(names.contains(&n), "{n} in {names:?}");
    }
    let config = recs.iter().find(|r| r["name"] == "config").unwrap();
    assert_eq!(config["parent"], "process");
    let spawn = recs.iter().find(|r| r["name"] == "spawn").unwrap();
    assert_eq!(spawn["parent"], "process", "the spawn is inside the run's root span");
    assert!(config["ms"].is_f64() && config["startMs"].is_f64(), "{config}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn claudes_own_debug_flag_alone_passes_through() {
    let d = scratch("debug-pass");
    fake_claude(&d);
    for debug_env in [false, true] {
        let mut c = launcher(&d);
        if debug_env {
            c.env("CLAUDE_AUTO_DEBUG", "1");
        }
        let o = c.arg("--debug").output().unwrap();
        let out = text(&o);
        assert_eq!(o.status.code(), Some(7), "{out}");
        assert!(out.contains("ARGV --debug"), "debug env {debug_env}: {out}");
    }
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn timings_without_a_log_prints_no_records_and_exits_zero() {
    let d = scratch("timings-empty");
    let o = launcher(&d).args(["--launcher-timings", "--days", "3"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(text(&o).trim_end(), "no timing records");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn preview_in_debug_mode_shows_the_timings_screen_and_the_footer_only_on_request() {
    let d = scratch("debug-preview");
    let config = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("config-four.json");
    let run = |flag: bool, env: Option<&str>| {
        let mut c = launcher(&d);
        c.env("CLAUDE_AUTO_CONFIG", &config)
            .env("CLAUDE_AUTO_PREVIEW", "1")
            .env("CLAUDE_AUTO_PREVIEW_KEYS", "F12,Escape,Escape");
        if flag {
            c.arg("--launcher-debug");
        }
        if let Some(v) = env {
            c.env("CLAUDE_AUTO_DEBUG", v);
        }
        text(&c.output().unwrap())
    };
    let on = run(true, None);
    assert!(on.contains("timings of this run"), "{on}");
    assert!(on.contains("screen:launch"), "{on}");
    assert!(!on.contains("last key->frame"), "plain debug draws no footer: {on}");
    let footer = run(false, Some("footer"));
    assert!(footer.contains("  load "), "{footer}");
    assert!(footer.contains("last key->frame"), "{footer}");
    assert!(footer.contains("timings of this run"), "{footer}");
    let off = run(false, None);
    assert!(!off.contains("last key->frame") && !off.contains("timings of this run"), "{off}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_run_that_never_starts_claude_still_logs_its_timings() {
    let d = scratch("debug-no-claude");
    let o = launcher(&d).env("CLAUDE_AUTO_DEBUG", "1").args(["-p", "x"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1), "no claude on PATH: {}", text(&o));
    let logs = d.join("home").join(".claude").join("launcher-logs");
    let file = std::fs::read_dir(&logs).unwrap().next().unwrap().unwrap().path();
    let log = std::fs::read_to_string(file).unwrap();
    let timing = |name: &str| {
        log.lines()
            .any(|l| l.contains("\"stage\":\"timing\"") && l.contains(&format!("\"name\":\"{name}\"")))
    };
    assert!(timing("process") && timing("prepare"), "{log}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn subcommand_gets_no_mcp_config() {
    let d = scratch("sub");
    fake_claude(&d);
    std::fs::write(d.join("home").join(".claude").join("mcp-shared.json"), "{}").unwrap();
    let o = launcher(&d).args(["mcp", "list"]).output().unwrap();
    let out = text(&o);
    assert!(out.contains("ARGV mcp list\n"), "{out}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn missing_claude_exits_one() {
    let d = scratch("missing");
    let o = launcher(&d).arg("--version").output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("claude is not on PATH - install Claude Code first"));
    std::fs::remove_dir_all(&d).unwrap();
}

/// Maps the Unicode goldens onto the ASCII glyph set, for a console not on UTF-8.
fn to_ascii(s: &str) -> String {
    let pairs = [
        ('\u{256D}', '+'),
        ('\u{256E}', '+'),
        ('\u{2570}', '+'),
        ('\u{256F}', '+'),
        ('\u{2500}', '-'),
        ('\u{2502}', '|'),
        ('\u{273B}', '*'),
        ('\u{276F}', '>'),
        ('\u{25CF}', '*'),
        ('\u{25CB}', '-'),
        ('\u{250A}', ':'),
    ];
    s.chars()
        .map(|c| pairs.iter().find(|(u, _)| *u == c).map_or(c, |(_, a)| *a))
        .collect()
}

#[test]
fn preview_dumps_the_launch_frame_and_launches_nothing() {
    let d = scratch("preview");
    fake_claude(&d);
    let config = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("config-four.json");
    let o = launcher(&d)
        .env("CLAUDE_AUTO_CONFIG", &config)
        .env("CLAUDE_AUTO_PREVIEW", "1")
        .env("CLAUDE_AUTO_PREVIEW_SIZE", "100x30")
        .env("CLAUDE_AUTO_PREVIEW_KEYS", "Enter,Enter")
        .output()
        .unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    let golden =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wide-frames/launch-100x30.txt")).unwrap();
    let golden = golden.replace("\r\n", "\n");
    let expected = if out.contains('\u{256D}') { golden } else { to_ascii(&golden) };
    assert!(out.starts_with(&expected), "frame differs:\n{out}");
    assert!(out.contains("--- preview, nothing launched ---"));
    assert!(out.contains("launch args        : \n"), "a default launch adds no arguments: {out}");
    assert!(!out.contains("ARGV"), "preview never starts claude");
    assert!(!d.join("prefs.json").exists(), "preview writes no prefs");
    assert!(
        !d.join("home").join(".claude").join("launcher-logs").exists(),
        "preview writes no log"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn preview_steps_rows_into_launch_arguments() {
    let d = scratch("steps");
    fake_claude(&d);
    let config = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("config-four.json");
    let o = launcher(&d)
        .env("CLAUDE_AUTO_CONFIG", &config)
        .env("CLAUDE_AUTO_PREVIEW", "1")
        .env("CLAUDE_AUTO_PREVIEW_KEYS", "RightArrow,s,s,s,d,s,d,d,d,d,s,d,Enter")
        .output()
        .unwrap();
    let out = text(&o);
    assert!(out.contains("account            : personal"), "{out}");
    assert!(
        out.contains("launch args        : --model fable --effort xhigh --advisor fable"),
        "{out}"
    );
    assert!(out.contains("CLAUDE_CONFIG_DIR  : "), "{out}");
    std::fs::remove_dir_all(&d).unwrap();
}

/// (local) Two accounts with separate stores, no remote: rows are account, action, then the habits.
/// `b` holds one session recorded in the launcher's working directory.
fn two_accounts(name: &str) -> PathBuf {
    let d = scratch(name);
    fake_claude(&d);
    let config = serde_json::json!({
        "accounts": [
            { "key": "alpha", "root": "~/.claude", "label": "alpha", "tint": "Green" },
            { "key": "beta", "root": "~/.claude-b", "label": "beta", "tint": "Magenta" }
        ],
        "sharing": false,
        "remote": false
    });
    std::fs::write(d.join("home").join("config.json"), config.to_string()).unwrap();
    let slug_dir = d.join("home").join(".claude-b").join("projects").join("SLUG");
    std::fs::create_dir_all(&slug_dir).unwrap();
    let record = serde_json::json!({
        "type": "user",
        "cwd": d.to_string_lossy(),
        "message": { "role": "user", "content": "the foreign prompt" }
    });
    std::fs::write(slug_dir.join("foreign-id.jsonl"), format!("{record}\n")).unwrap();
    std::fs::create_dir_all(d.join("home").join(".claude").join("projects")).unwrap();
    d
}

fn preview_two(d: &Path, keys: &str) -> String {
    let o = launcher(d)
        .env("CLAUDE_AUTO_CONFIG", d.join("home").join("config.json"))
        .env("CLAUDE_AUTO_PREVIEW", "1")
        .env("CLAUDE_AUTO_PREVIEW_KEYS", keys)
        .output()
        .unwrap();
    text(&o)
}

#[test]
fn enter_on_the_launch_screen_starts_in_the_launch_directory() {
    let d = two_accounts("direct");
    let out = preview_two(&d, "Enter");
    assert!(out.contains("--- preview, nothing launched ---"), "one Enter launches: {out}");
    assert!(!out.contains("current directory"), "no project screen: {out}");
    assert!(
        out.contains("action") && out.contains("[new]") && out.contains("project"),
        "the row is on the screen: {out}"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn the_project_action_opens_the_project_screen_over_every_account() {
    let d = two_accounts("project");
    let out = preview_two(&d, "s,d,Enter,Escape");
    assert!(out.contains("current directory"), "the project screen opened: {out}");
    assert!(out.contains("--- preview: picker cancelled ---"), "{out}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn resume_lists_another_accounts_session_and_preview_copies_nothing() {
    let d = two_accounts("transfer");
    let out = preview_two(&d, "s,d,d,d,Enter,Enter");
    assert!(out.contains("[beta]"), "the row names its owner: {out}");
    assert!(
        out.contains("session transfer   : foreign-id from beta -> alpha (copied on a real launch)"),
        "{out}"
    );
    assert!(out.contains("launch args        : --resume foreign-id"), "{out}");
    assert!(
        !d.join("home").join(".claude").join("projects").join("SLUG").exists(),
        "preview writes nothing"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

// (local) `--launcher-options` / `--launcher-run`. CLAUDE_BRIDGE_RUST=1 over a fake home is what makes
// these safe to run: the bridge is then a file in that home that is not there, so the launcher can
// neither start nor stop the real one, whatever the code under test decides.

fn phone_launcher(dir: &Path) -> Command {
    std::fs::write(
        dir.join("home").join("config.json"),
        r#"{"accounts":[{"key":"main","root":"~/.claude","label":"main acc","tint":"Green"},{"key":"alt","root":"~/.claude-alt","label":"alt acc","tint":"Magenta"}],"remote":true,"riderMcp":"off"}"#,
    )
    .unwrap();
    let mut c = launcher(dir);
    c.env("CLAUDE_AUTO_CONFIG", dir.join("home").join("config.json"))
        .env("CLAUDE_BRIDGE_RUST", "1")
        .env_remove("CLAUDE_BRIDGE_ROOT");
    c
}

/// A fake `claude` that also says where it was started and leaves a file behind.
fn fake_claude_with_cwd(dir: &Path) {
    let marker = dir.join("claude-started");
    std::fs::write(
        dir.join("bin").join("claude.cmd"),
        format!(
            "@echo ARGV %*\r\n@echo CWD [%CD%]\r\n@echo CONFIG [%CLAUDE_CONFIG_DIR%]\r\n@echo. > \"{}\"\r\n@exit /b 7\r\n",
            marker.display()
        ),
    )
    .unwrap();
}

fn run_line(project: &Path, tail: &[&str]) -> Vec<String> {
    let project = project.to_string_lossy().into_owned();
    let mut line: Vec<String> = [
        "--launcher-run",
        "--account",
        "main",
        "--project",
        project.as_str(),
        "--model",
        "haiku",
        "--effort",
        "high",
        "--advisor",
        "off",
        "--permission",
        "auto",
        "--mode",
        "safe",
        "--action",
        "new",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    for pair in tail.chunks(2) {
        let (flag, value) = (pair[0], pair[1]);
        if let Some(i) = line.iter().position(|a| a == flag) {
            value.clone_into(&mut line[i + 1]);
        } else {
            line.extend([flag.to_owned(), value.to_owned()]);
        }
    }
    line
}

fn logs_dir(dir: &Path) -> PathBuf {
    dir.join("home").join(".claude").join("launcher-logs")
}

#[test]
fn launcher_options_prints_one_line_of_json_and_leaves_no_trace() {
    let d = scratch("options");
    fake_claude_with_cwd(&d);
    let prefs_text = r#"{"Version":2,"Account":"alt","Profiles":{"main":{"Effort":"medium","Remote":"off"}}}"#;
    std::fs::write(d.join("prefs.json"), prefs_text).unwrap();
    let rate = d.join("home").join(".claude").join("rate-limits");
    std::fs::create_dir_all(&rate).unwrap();
    std::fs::write(rate.join("main.json"), r#"{"fiveHour":16,"sevenDay":3,"atMs":1800000000000}"#).unwrap();
    let versions = d.join("home").join(".local").join("share").join("claude").join("versions");
    std::fs::create_dir_all(&versions).unwrap();
    std::fs::write(versions.join("2.1.99"), "x").unwrap();

    let started = std::time::Instant::now();
    let o = phone_launcher(&d).arg("--launcher-options").output().unwrap();
    let took = started.elapsed();

    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(took.as_secs_f64() < 3.0, "the bridge gives this 5 s and the owner 3: took {took:?}");
    let out = text(&o);
    assert_eq!(out.trim_end().lines().count(), 1, "one line, nothing else on stdout: {out}");
    let v: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
    assert_eq!(v["launcher"], "claude-auto");
    assert_eq!(v["version"], "2.1.99");
    assert_eq!(v["accounts"][0]["key"], "main");
    assert_eq!(v["accounts"][0]["limitPct"], 16);
    assert_eq!(v["accounts"][0]["prefs"]["effort"], "medium");
    assert_eq!(v["accounts"][1]["limitPct"], serde_json::Value::Null);

    assert!(
        !d.join("claude-started").exists(),
        "the catalog and the version come without starting claude"
    );
    assert_eq!(
        std::fs::read_to_string(d.join("prefs.json")).unwrap(),
        prefs_text,
        "prefs stay as they were"
    );
    assert!(!logs_dir(&d).exists(), "no start log");
    assert!(
        !d.join("home").join(".claude").join("claude-auto-models.json").exists(),
        "a cold catalog is not scanned"
    );
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn launcher_options_keeps_config_warnings_off_the_json() {
    let d = scratch("options-warn");
    let mut c = phone_launcher(&d);
    std::fs::write(d.join("home").join("config.json"), r#"{"nonsense":1,"remote":true}"#).unwrap();
    c.env("CLAUDE_AUTO_CONFIG", d.join("home").join("config.json"));
    let o = c.arg("--launcher-options").output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&o.stderr).is_empty(), "the warning is still said");
    let _: serde_json::Value = serde_json::from_str(text(&o).trim_end()).unwrap();
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn launcher_options_refuses_extra_arguments_rather_than_passing_them_to_claude() {
    let d = scratch("options-extra");
    fake_claude_with_cwd(&d);
    let o = phone_launcher(&d).args(["--launcher-options", "--x"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(!d.join("claude-started").exists());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn launcher_run_refuses_a_bad_request_before_any_side_effect() {
    let d = scratch("run-bad");
    fake_claude_with_cwd(&d);
    let prefs_text = r#"{"Version":2,"Profiles":{"main":{"Remote":"off"}}}"#;
    std::fs::write(d.join("prefs.json"), prefs_text).unwrap();
    let missing = d.join("no-such-dir");
    let missing = missing.to_string_lossy();
    for (name, tail) in [
        ("unknown account", vec!["--account", "nobody"]),
        ("value outside the row", vec!["--model", "gpt"]),
        ("the project action", vec!["--action", "project"]),
        ("missing directory", vec!["--project", &*missing]),
        ("resume without an id", vec!["--action", "resume"]),
        ("unknown flag", vec!["--whatever", "1"]),
    ] {
        let o = phone_launcher(&d).args(run_line(&d, &tail)).output().unwrap();
        assert_eq!(o.status.code(), Some(2), "{name}");
        assert!(!String::from_utf8_lossy(&o.stderr).is_empty(), "{name}: says why");
        assert!(!d.join("claude-started").exists(), "{name}: claude did not start");
        assert!(!logs_dir(&d).exists(), "{name}: no log was written");
        assert_eq!(
            std::fs::read_to_string(d.join("prefs.json")).unwrap(),
            prefs_text,
            "{name}: prefs untouched"
        );
    }
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn launcher_run_starts_claude_in_the_project_and_keeps_the_remembered_remote() {
    let d = scratch("run");
    fake_claude_with_cwd(&d);
    let project = d.join("the project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        d.join("prefs.json"),
        r#"{"Version":2,"Profiles":{"alt":{"Remote":"off","Model":"fable"}}}"#,
    )
    .unwrap();

    let o = phone_launcher(&d)
        .args(run_line(
            &project,
            &["--account", "alt", "--action", "resume", "--resume-id", "abc"],
        ))
        .arg("--fork")
        .output()
        .unwrap();
    let out = text(&o);

    assert_eq!(o.status.code(), Some(7), "claude's own code comes back: {out}");
    assert!(
        out.contains("--model haiku --effort high --permission-mode auto --safe-mode --resume abc --fork-session"),
        "the screen's arguments: {out}"
    );
    assert!(out.contains(&format!("CWD [{}]", project.display())), "{out}");
    assert!(out.contains(r".claude-alt]"), "the account's profile: {out}");
    assert!(
        out.contains("remote: no installed bridge"),
        "the saved `off` still takes the ensure path, which alone reports this: {out}"
    );

    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.join("prefs.json")).unwrap()).unwrap();
    assert_eq!(saved["Account"], "alt");
    assert_eq!(
        saved["Profiles"]["alt"]["Remote"], "off",
        "the phone does not rewrite the owner's switch"
    );
    assert_eq!(saved["Profiles"]["alt"]["Model"], "haiku", "the phone's choice is remembered");
    assert!(logs_dir(&d).exists(), "the launch logs as one from the screen does");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn launcher_run_does_not_write_a_remote_the_owner_never_chose() {
    // Запуск с телефона на профиле без запомненной строки: экранное сохранение записало бы `on`.
    let d = scratch("run-no-remote");
    fake_claude_with_cwd(&d);
    std::fs::write(d.join("prefs.json"), r#"{"Version":2,"Profiles":{"main":{"Model":"fable"}}}"#).unwrap();
    let o = phone_launcher(&d).args(run_line(&d, &[])).output().unwrap();
    assert_eq!(o.status.code(), Some(7), "{}", text(&o));
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.join("prefs.json")).unwrap()).unwrap();
    assert_eq!(saved["Profiles"]["main"]["Model"], "haiku");
    assert!(saved["Profiles"]["main"].get("Remote").is_none(), "{saved}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn launcher_run_in_preview_goes_the_same_way_and_launches_and_writes_nothing() {
    let d = scratch("run-preview");
    fake_claude_with_cwd(&d);
    let prefs_text = r#"{"Version":2,"Profiles":{"main":{"Remote":"off"}}}"#;
    std::fs::write(d.join("prefs.json"), prefs_text).unwrap();
    let o = phone_launcher(&d)
        .env("CLAUDE_AUTO_PREVIEW", "1")
        .args(run_line(&d, &["--action", "worktree"]))
        .output()
        .unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(out.contains("--- preview, nothing launched ---"), "no screen first: {out}");
    assert!(out.contains("account            : main"), "{out}");
    assert!(out.contains(&format!("project            : {}", d.display())), "{out}");
    assert!(out.contains("remote             : True"), "the saved off does not switch it: {out}");
    assert!(
        out.contains("--model haiku --effort high --permission-mode auto --safe-mode -w"),
        "{out}"
    );
    assert!(out.contains("disabled (CLAUDE_CODE_DISABLE_ADVISOR_TOOL=1)"), "{out}");
    assert!(!d.join("claude-started").exists());
    assert_eq!(std::fs::read_to_string(d.join("prefs.json")).unwrap(), prefs_text);
    std::fs::remove_dir_all(&d).unwrap();
}
