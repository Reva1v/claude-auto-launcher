use super::*;

const ROOT: &str = r"C:\Users\someone\Projects\claude-tg-bridge";

/// Records what the stop seam saw.
struct Fake {
    listener: Option<u32>,
    info: Option<ProcInfo>,
    fail: Option<String>,
    stopped: Vec<u32>,
}

impl Procs for Fake {
    fn listener(&mut self, _port: u16) -> Option<u32> {
        self.listener
    }
    fn info(&mut self, _pid: u32) -> Option<ProcInfo> {
        self.info.clone()
    }
    fn stop(&mut self, pid: u32) -> Result<(), String> {
        self.stopped.push(pid);
        self.fail.clone().map_or(Ok(()), Err)
    }
}

fn fake(listener: Option<u32>, name: &str, cmd: &str) -> Fake {
    Fake {
        listener,
        info: (!name.is_empty()).then(|| ProcInfo {
            name: name.into(),
            command_line: cmd.into(),
            image: None,
        }),
        fail: None,
        stopped: Vec::new(),
    }
}

fn stop(f: &mut Fake) -> Option<String> {
    stop_with(&Bridge::Checkout(ROOT.into()), 8787, f)
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("claude-auto-rs-bridge-{tag}-{}", crate::log::new_run_id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn nothing_listening_stops_nothing_and_says_nothing() {
    let mut f = fake(None, "", "");
    assert_eq!(stop(&mut f), None);
    assert_eq!(f.stopped, Vec::<u32>::new());
}

#[test]
fn a_dead_pid_is_not_stopped() {
    let mut f = fake(Some(4242), "", "");
    assert_eq!(stop(&mut f), None);
    assert_eq!(f.stopped, Vec::<u32>::new());
}

#[test]
fn another_process_on_the_port_survives_off() {
    let mut f = fake(Some(13308), "tsh", "tsh proxy app unic-adapter --port 8787");
    assert_eq!(stop(&mut f), None);
    assert_eq!(f.stopped, Vec::<u32>::new());
}

#[test]
fn a_node_from_another_checkout_is_left_alone() {
    let mut f = fake(
        Some(1),
        "node",
        r"node C:\Users\someone\Projects\claude-tg-bridge-old\dist\index.js",
    );
    stop(&mut f);
    assert_eq!(f.stopped, Vec::<u32>::new());
}

#[test]
fn a_non_node_process_is_not_stopped_even_from_the_right_directory() {
    let mut f = fake(Some(1), "python", &format!(r"python {ROOT}\dist\serve.py"));
    stop(&mut f);
    assert_eq!(f.stopped, Vec::<u32>::new());
}

#[test]
fn an_unreadable_command_line_is_not_a_match() {
    let mut f = fake(Some(1), "node", "");
    stop(&mut f);
    assert_eq!(f.stopped, Vec::<u32>::new());
}

#[test]
fn the_real_bridge_is_stopped() {
    let mut f = fake(Some(777), "node", &format!(r"node {ROOT}\dist\index.js"));
    assert!(stop(&mut f).unwrap().contains("stopped"));
    assert_eq!(f.stopped, vec![777]);
}

#[test]
fn forward_slashes_and_case_still_match_the_root() {
    let mut f = fake(Some(1), "Node", "node c:/users/someone/projects/claude-tg-bridge/dist/index.js");
    stop(&mut f);
    assert_eq!(f.stopped.len(), 1);
}

#[test]
fn a_failing_stop_is_a_status_line() {
    let mut f = fake(Some(1), "node", &format!(r"node {ROOT}\dist\index.js"));
    f.fail = Some("access is denied".into());
    assert!(stop(&mut f).unwrap().contains("access is denied"));
}

#[test]
fn root_resolution() {
    let home = Path::new(r"C:\Users\x");
    assert_eq!(bridge_root(Some(r"D:\p\bridge"), home), PathBuf::from(r"D:\p\bridge"));
    assert_eq!(bridge_root(Some(""), home), home.join("claude-tg-bridge"));
    assert_eq!(bridge_root(None, home), home.join("claude-tg-bridge"));
}

#[test]
fn env_values_skip_comments_blanks_and_quotes() {
    let env = "# BRIDGE_USER_ID=1\nBRIDGE_BOT_TOKEN=\nBRIDGE_USER_ID= \"42\" \n";
    assert_eq!(env_value(env, "BRIDGE_BOT_TOKEN"), None, "an empty token means not set up");
    assert_eq!(env_value(env, "BRIDGE_USER_ID").as_deref(), Some("42"));
    assert_eq!(env_value(env, "MISSING"), None);
}

#[test]
fn the_port_comes_from_the_env_file_or_the_bridge_default() {
    assert_eq!(bridge_port("BRIDGE_PORT=8792\n"), 8792);
    assert_eq!(bridge_port("# BRIDGE_PORT=8792\n"), 8787, "a commented line is not a setting");
    assert_eq!(bridge_port("BRIDGE_PORT=nope\n"), 8787);
    assert_eq!(bridge_port("BRIDGE_PORT=0\n"), 8787);
}

#[test]
fn a_bridge_without_a_token_is_ready_it_runs_as_the_app() {
    let d = temp_dir("ready");
    assert!(not_ready(&d).unwrap().contains("no bridge checkout"));
    std::fs::write(d.join("package.json"), "{}").unwrap();
    assert!(not_ready(&d).unwrap().contains("npm install"));
    std::fs::create_dir(d.join("node_modules")).unwrap();
    assert_eq!(not_ready(&d), None, "no .env at all still starts: no token means app mode");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_stale_dist_is_rebuilt_and_a_fresh_one_is_not() {
    let d = temp_dir("dist");
    std::fs::create_dir_all(d.join("src").join("lib")).unwrap();
    std::fs::create_dir_all(d.join("dist")).unwrap();
    std::fs::write(d.join("package.json"), r#"{"scripts":{"build":"tsc"}}"#).unwrap();
    std::fs::write(d.join("src").join("lib").join("a.ts"), "x").unwrap();
    assert!(dist_stale(&d), "no dist is stale");
    std::fs::write(d.join("dist").join("index.js"), "x").unwrap();
    let later = SystemTime::now() + Duration::from_secs(60);
    File::options()
        .write(true)
        .open(d.join("dist").join("index.js"))
        .unwrap()
        .set_modified(later)
        .unwrap();
    assert!(!dist_stale(&d), "a dist newer than every source is current");
    File::options()
        .write(true)
        .open(d.join("src").join("lib").join("a.ts"))
        .unwrap()
        .set_modified(later + Duration::from_secs(60))
        .unwrap();
    assert!(dist_stale(&d), "a nested source newer than dist makes it stale");
    assert!(has_build_script(&d));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_failed_build_keeps_the_previous_dist_but_not_nothing() {
    let d = temp_dir("failed");
    std::fs::write(d.join("package.json"), "{}").unwrap();
    let log = d.join("build.log");
    assert!(prepare_dist(&d, &log).is_err(), "no build script and no dist: nothing to run");
    std::fs::create_dir(d.join("dist")).unwrap();
    std::fs::write(d.join("dist").join("index.js"), "x").unwrap();
    File::options()
        .write(true)
        .open(d.join("package.json"))
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(60))
        .unwrap();
    assert!(dist_stale(&d));
    assert!(prepare_dist(&d, &log).is_ok(), "an old bridge beats no bridge");
    std::fs::remove_dir_all(&d).unwrap();
}

const EXE: &str = r"C:\Users\someone\.claude-tg-bridge\bin\claude-bridge.exe";

fn installed() -> Bridge {
    Bridge::Installed(EXE.into())
}

/// Процесс на порту с заданным файлом: имя и аргументы выбраны так, чтобы путать опознание.
fn holder(image: Option<&str>, name: &str, cmd: &str) -> Fake {
    let mut f = fake(Some(555), name, cmd);
    if let Some(info) = f.info.as_mut() {
        info.image = image.map(PathBuf::from);
    }
    f
}

fn stops(bridge: &Bridge, f: &mut Fake) -> bool {
    stop_with(bridge, 8793, f);
    !f.stopped.is_empty()
}

#[test]
fn a_copy_on_disk_does_not_switch_the_launcher_only_the_explicit_flag_does() {
    let home = temp_dir("choose");
    let exe = installed_exe(&home);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "x").unwrap();
    let old = Bridge::Checkout(PathBuf::from(r"D:\p\bridge"));
    for flag in [None, Some(""), Some("0"), Some("true")] {
        assert_eq!(choose_bridge(flag, Some(r"D:\p\bridge"), &home), old, "{flag:?}");
    }
    assert_eq!(choose_bridge(Some("1"), Some(r"D:\p\bridge"), &home), Bridge::Installed(exe));
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn the_installed_path_is_the_one_setup_writes() {
    assert_eq!(
        installed_exe(Path::new(r"C:\Users\someone")),
        PathBuf::from(EXE),
        "contracts/cli-and-api.md: ~/.claude-tg-bridge/bin/"
    );
}

#[test]
fn the_installed_copy_is_stopped_whatever_it_is_called_or_was_started_with() {
    assert!(stops(&installed(), &mut holder(Some(EXE), "claude-bridge", "claude-bridge serve")));
    assert!(
        stops(&installed(), &mut holder(Some(EXE), "x", "")),
        "an unreadable command line changes nothing"
    );
    let lower = EXE.replace('\\', "/").to_lowercase();
    assert!(
        stops(&installed(), &mut holder(Some(&lower), "x", "")),
        "Windows paths ignore case and slash"
    );
}

#[test]
fn a_bridge_built_elsewhere_survives_off_even_with_the_same_name() {
    let dev = r"D:\Projects\claude-tg-bridge\target\release\claude-bridge.exe";
    assert!(!stops(&installed(), &mut holder(Some(dev), "claude-bridge", "claude-bridge serve")));
}

#[test]
fn naming_the_installed_path_in_arguments_does_not_make_a_process_the_bridge() {
    let cmd = format!(r"pwsh -c Get-Item {EXE}; {EXE} serve");
    let pwsh = r"C:\Program Files\PowerShell\7\pwsh.exe";
    assert!(!stops(&installed(), &mut holder(Some(pwsh), "claude-bridge", &cmd)));
}

#[test]
fn an_unreadable_image_path_is_not_a_match_it_might_be_anyone() {
    assert!(!stops(&installed(), &mut holder(None, "claude-bridge", &format!("{EXE} serve"))));
}

#[test]
fn only_exact_files_match_not_a_path_that_starts_the_same() {
    for image in [
        format!("{EXE}.bak"),
        format!("{EXE}.new"),
        format!("{EXE}.old-"),
        format!("{EXE}.old-x"),
        format!(r"{EXE}.old-1\claude-bridge.exe"),
        EXE.replacen("C:", "D:", 1),
        EXE.replace("someone", "someone-else"),
    ] {
        assert!(!stops(&installed(), &mut holder(Some(&image), "x", "")), "{image}");
    }
}

#[test]
fn a_running_copy_retired_by_setup_is_still_stopped_off_must_reach_it() {
    let retired = format!("{EXE}.old-3");
    assert!(stops(&installed(), &mut holder(Some(&retired), "claude-bridge.exe.old-3", "")));
}

#[test]
fn the_node_bridge_is_not_stopped_by_the_new_identity_and_the_new_one_not_by_the_old() {
    let node_cmd = format!(r"node {ROOT}\dist\index.js");
    let node_image = r"C:\Program Files\nodejs\node.exe";
    assert!(!stops(&installed(), &mut holder(Some(node_image), "node", &node_cmd)));
    assert!(!stops(
        &Bridge::Checkout(ROOT.into()),
        &mut holder(Some(EXE), "claude-bridge", &format!("{EXE} serve"))
    ));
    assert!(stops(
        &Bridge::Checkout(ROOT.into()),
        &mut holder(Some(node_image), "node", &node_cmd)
    ));
}

/// `ensure_with` над настоящим файлом в temp и поддельными процессами; сколько раз звали запуск.
fn ensure(f: &mut Fake, start: &Result<String, String>) -> (Option<String>, u32) {
    let home = temp_dir("ensure");
    let exe = installed_exe(&home);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "x").unwrap();
    let mut starts = 0;
    let status = ensure_with(&Bridge::Installed(exe), 8793, f, &mut || {
        starts += 1;
        start.clone()
    });
    std::fs::remove_dir_all(&home).unwrap();
    (status, starts)
}

#[test]
fn on_with_no_installed_copy_says_what_to_run_and_starts_nothing() {
    let home = temp_dir("missing");
    let bridge = Bridge::Installed(installed_exe(&home));
    let mut starts = 0;
    let status = ensure_with(&bridge, 8793, &mut fake(None, "", ""), &mut || {
        starts += 1;
        Ok(String::new())
    });
    assert!(status.unwrap().contains("claude-bridge setup"));
    assert_eq!(starts, 0, "a session must still start, with no bridge rather than a broken one");
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn on_starts_the_copy_once_when_the_port_is_free_and_a_failure_is_a_status_line() {
    assert_eq!(ensure(&mut fake(None, "", ""), &Ok("up".into())), (Some("up".into()), 1));
    let (status, starts) = ensure(&mut fake(None, "", ""), &Err("access is denied".into()));
    assert_eq!(starts, 1);
    assert!(status.unwrap().contains("access is denied"));
}

#[test]
fn on_leaves_a_running_copy_alone_and_says_nothing() {
    let (status, starts) = ensure_running();
    assert_eq!((status, starts), (None, 0), "a second session must not start a second bridge");
}

/// Копия уже слушает порт: путь берётся настоящий, потому что `ensure` кладёт файл в temp.
fn ensure_running() -> (Option<String>, u32) {
    let home = temp_dir("running");
    let exe = installed_exe(&home);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "x").unwrap();
    let image = exe.to_string_lossy().into_owned();
    let mut f = holder(Some(&image), "claude-bridge", "");
    let mut starts = 0;
    let status = ensure_with(&Bridge::Installed(exe), 8793, &mut f, &mut || {
        starts += 1;
        Ok(String::new())
    });
    assert_eq!(f.stopped, Vec::<u32>::new());
    std::fs::remove_dir_all(&home).unwrap();
    (status, starts)
}

#[test]
fn on_with_someone_else_on_the_port_names_him_and_starts_nothing() {
    let mut f = holder(Some(r"C:\tsh\tsh.exe"), "tsh", "tsh proxy");
    let (status, starts) = ensure(&mut f, &Ok(String::new()));
    let status = status.unwrap();
    assert!(status.contains("tsh") && status.contains("BRIDGE_PORT"), "{status}");
    assert_eq!(starts, 0);
}

#[test]
fn the_port_of_the_installed_copy_comes_from_the_env_file_beside_it() {
    assert_eq!(
        installed().env_dir(),
        Path::new(r"C:\Users\someone\.claude-tg-bridge\bin"),
        "the bridge reads .env beside the exe first; the checkout's .env is the old bridge's port"
    );
}

#[test]
fn a_notepad_bom_or_export_does_not_lose_the_port() {
    assert_eq!(bridge_port("\u{feff}BRIDGE_PORT=8793\n"), 8793);
    assert_eq!(bridge_port("export BRIDGE_PORT=8793\n"), 8793);
}

#[test]
fn serve_runs_in_the_exe_directory_on_the_port_the_launcher_will_look_at() {
    let c = serve_command(Path::new(EXE), 8793);
    assert_eq!(c.get_program(), Path::new(EXE).as_os_str());
    assert_eq!(c.get_args().collect::<Vec<_>>(), ["serve"]);
    assert_eq!(
        c.get_current_dir(),
        Path::new(EXE).parent(),
        "the launcher's cwd is a project: the bridge would read that project's .env"
    );
    let port = c.get_envs().find(|(k, _)| *k == "BRIDGE_PORT").and_then(|(_, v)| v);
    assert_eq!(
        port,
        Some(std::ffi::OsStr::new("8793")),
        "the environment beats .env in the bridge: a leftover value would move it off the port"
    );
}
