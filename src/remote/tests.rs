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
        }),
        fail: None,
        stopped: Vec::new(),
    }
}

fn stop(f: &mut Fake) -> Option<String> {
    stop_with(Path::new(ROOT), 8787, f)
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
