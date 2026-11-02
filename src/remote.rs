//! (local) The `remote` row: claude-tg-bridge, started on demand and stopped only when it is
//! provably ours. The bridge attaches to every live `claude` by itself, so a session is launched
//! the same way with the row on or off.

mod win;

use std::fs::File;
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::out::{self, Fg};

/// The bridge's own default for `BRIDGE_PORT`.
const DEFAULT_PORT: u16 = 8787;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// A value of the bridge's `.env`; blank counts as missing.
fn env_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        // Блокнот сохраняет `.env` с BOM, а новый мост его понимает: иначе лаунчер искал бы мост на
        // порту по умолчанию и навязал бы ему этот порт при запуске.
        let line = line.trim_start_matches('\u{feff}').trim();
        let (k, v) = line.strip_prefix("export ").unwrap_or(line).split_once('=')?;
        (k.trim() == key)
            .then(|| v.trim().trim_matches(['"', '\'']).to_owned())
            .filter(|v| !v.is_empty())
    })
}

fn bridge_port(env: &str) -> u16 {
    env_value(env, "BRIDGE_PORT")
        .and_then(|p| p.parse().ok())
        .filter(|p| *p != 0)
        .unwrap_or(DEFAULT_PORT)
}

/// `CLAUDE_BRIDGE_ROOT` (the shims set it) when set, else a checkout in the home directory.
fn bridge_root(env_root: Option<&str>, home: &Path) -> PathBuf {
    match env_root.filter(|r| !r.is_empty()) {
        Some(r) => PathBuf::from(r),
        None => home.join("claude-tg-bridge"),
    }
}

/// Копия, которую ставит `claude-bridge setup`; путь не берётся из окружения, потому что лаунчер
/// запускает этот файл и останавливает то, что им считает.
fn installed_exe(home: &Path) -> PathBuf {
    home.join(".claude-tg-bridge").join("bin").join("claude-bridge.exe")
}

/// Какой мост ведёт строка `remote`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Bridge {
    /// Node из checkout-а: прежнее поведение.
    Checkout(PathBuf),
    /// Установленная копия `claude-bridge.exe`.
    Installed(PathBuf),
}

/// Только явный `CLAUDE_BRIDGE_RUST=1`, а не наличие файла: копия появляется при `claude-bridge
/// setup`, задолго до живой проверки, и лаунчер не должен перескочить на неё сам (T042).
fn choose_bridge(rust: Option<&str>, env_root: Option<&str>, home: &Path) -> Bridge {
    if rust == Some("1") {
        Bridge::Installed(installed_exe(home))
    } else {
        Bridge::Checkout(bridge_root(env_root, home))
    }
}

/// Только сам файл копии или он же, отставленный `claude-bridge setup`. Имя и аргументы не в счёт:
/// под этим именем ходит сборка из репозитория, а путь в аргументах бывает у кого угодно.
/// Отставленный считается, потому что путь идущего процесса следует за переименованием, а `setup`
/// переименовывает занятый exe: без этого `off` терял бы мост после каждой установки.
fn is_installed_copy(image: &Path, exe: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/").to_lowercase();
    let (image, exe) = (norm(image), norm(exe));
    image == exe
        || image
            .strip_prefix(&format!("{exe}.old-"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn live_bridge() -> Bridge {
    let home = crate::config::PathCtx::from_env().home;
    choose_bridge(
        std::env::var("CLAUDE_BRIDGE_RUST").ok().as_deref(),
        std::env::var("CLAUDE_BRIDGE_ROOT").ok().as_deref(),
        &home,
    )
}

impl Bridge {
    /// Где лежит `.env` этого моста.
    fn env_dir(&self) -> &Path {
        match self {
            Self::Checkout(root) => root,
            Self::Installed(exe) => exe.parent().unwrap_or(exe),
        }
    }

    fn owns(&self, info: &ProcInfo) -> bool {
        match self {
            Self::Checkout(root) => is_bridge(info, root),
            Self::Installed(exe) => info.image.as_deref().is_some_and(|image| is_installed_copy(image, exe)),
        }
    }

    /// `None`, когда мост можно запустить; иначе почему нельзя и что сделать.
    fn missing(&self) -> Option<String> {
        match self {
            Self::Checkout(root) => not_ready(root),
            Self::Installed(exe) => (!exe.is_file()).then(|| format!("no installed bridge at {}: run claude-bridge setup", exe.display())),
        }
    }
}

fn read_env(root: &Path) -> String {
    std::fs::read_to_string(root.join(".env")).unwrap_or_default()
}

/// A process as the stop guard sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcInfo {
    /// Without `.exe`, like `Get-Process`'s `ProcessName`.
    name: String,
    command_line: String,
    /// Полный путь исполняемого файла; `None`, когда его нельзя прочитать.
    image: Option<PathBuf>,
}

/// Node running this checkout's `dist`. The port alone says nothing about identity: here 8787 is
/// often someone else's proxy.
fn is_bridge(info: &ProcInfo, root: &Path) -> bool {
    if !info.name.eq_ignore_ascii_case("node") {
        return false;
    }
    let root = root.to_string_lossy().replace('\\', "/");
    let needle = format!("{}/dist/", root.trim_end_matches('/')).to_lowercase();
    root.len() > 1 && info.command_line.replace('\\', "/").to_lowercase().contains(&needle)
}

/// The process seams of the stop path, so tests never find or stop a real process.
trait Procs {
    fn listener(&mut self, port: u16) -> Option<u32>;
    fn info(&mut self, pid: u32) -> Option<ProcInfo>;
    fn stop(&mut self, pid: u32) -> Result<(), String>;
}

#[derive(Debug)]
struct LiveProcs;

impl Procs for LiveProcs {
    fn listener(&mut self, port: u16) -> Option<u32> {
        win::listener_pid(port)
    }

    fn info(&mut self, pid: u32) -> Option<ProcInfo> {
        let row = crate::sys::process_table().into_iter().find(|r| r.pid == pid)?;
        let name = row
            .name
            .strip_suffix(".exe")
            .or_else(|| row.name.strip_suffix(".EXE"))
            .unwrap_or(&row.name)
            .to_owned();
        Some(ProcInfo {
            name,
            command_line: win::command_line(pid).unwrap_or_default(),
            image: win::image_path(pid),
        })
    }

    /// The whole tree: the bridge owns its `cloudflared`, which would outlive a lone kill and keep
    /// the old tunnel up.
    fn stop(&mut self, pid: u32) -> Result<(), String> {
        let done = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|e| e.to_string())?;
        if done.success() {
            Ok(())
        } else {
            Err(format!("taskkill exited with {done}"))
        }
    }
}

/// A status line only when something happened: `off` is remembered, and an idle launch stays quiet.
fn stop_with(bridge: &Bridge, port: u16, procs: &mut dyn Procs) -> Option<String> {
    let pid = procs.listener(port)?;
    let info = procs.info(pid)?;
    if !bridge.owns(&info) {
        return None;
    }
    Some(match procs.stop(pid) {
        Ok(()) => format!("bridge stopped (pid {pid})"),
        Err(e) => format!("failed to stop the bridge (pid {pid}): {e}"),
    })
}

/// The `off` value: stops the bridge if it is running.
pub fn stop_bridge() -> Option<String> {
    let bridge = live_bridge();
    stop_with(&bridge, bridge_port(&read_env(bridge.env_dir())), &mut LiveProcs)
}

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    crate::invocation::find_on_path(name, &path, &pathext)
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

fn newest_ts(dir: &Path) -> Option<SystemTime> {
    let mut newest = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let t = if p.is_dir() {
            newest_ts(&p)
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ts")) {
            mtime(&p)
        } else {
            None
        };
        newest = newest.max(t);
    }
    newest
}

/// `dist/index.js` is missing or older than any source or config file.
fn dist_stale(root: &Path) -> bool {
    let mut newest = newest_ts(&root.join("src"));
    for extra in ["package.json", "tsconfig.json", "tsconfig.build.json"] {
        newest = newest.max(mtime(&root.join(extra)));
    }
    match mtime(&root.join("dist").join("index.js")) {
        None => true,
        Some(d) => newest.is_some_and(|n| d < n),
    }
}

fn has_build_script(root: &Path) -> bool {
    std::fs::read_to_string(root.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(t.trim_start_matches('\u{feff}')).ok())
        .and_then(|pkg| pkg.pointer("/scripts/build").and_then(Value::as_str).map(|s| !s.is_empty()))
        .unwrap_or(false)
}

/// `npm run build` in `root`, its output in `build_log`. Not the session's environment: the bridge
/// outlives this launch and must not inherit one project's secrets.
fn run_build(npm: &Path, root: &Path, build_log: &Path) -> Result<bool, std::io::Error> {
    let f = File::create(build_log)?;
    let f2 = f.try_clone()?;
    Ok(Command::new(npm)
        .args(["run", "build", "--silent"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(f)
        .stderr(f2)
        .status()?
        .success())
}

/// Rebuilds a stale `dist`. A failed build keeps an existing `dist` running: an old bridge beats no
/// bridge. `Err` only when there is nothing to run.
fn prepare_dist(root: &Path, build_log: &Path) -> Result<(), String> {
    let built = root.join("dist").join("index.js");
    if !dist_stale(root) {
        return Ok(());
    }
    let failure = if has_build_script(root) {
        let npm = on_path("npm").ok_or("npm is not on PATH")?;
        match run_build(&npm, root, build_log) {
            Ok(true) => return Ok(()),
            Ok(false) => format!("build failed, see {}", build_log.display()),
            Err(e) => format!("build could not run: {e}"),
        }
    } else {
        "no build script in package.json".to_owned()
    };
    if built.exists() {
        out::line(&format!("  remote: {failure}; starting the previous build"), Fg::DarkYellow);
        Ok(())
    } else {
        Err(failure)
    }
}

/// `None` when the bridge can be started; otherwise why it cannot, with the next step.
fn not_ready(root: &Path) -> Option<String> {
    if !root.join("package.json").exists() {
        return Some(format!("no bridge checkout at {}", root.display()));
    }
    if !root.join("node_modules").exists() {
        return Some(format!("bridge is not set up: run npm install in {}", root.display()));
    }
    None
}

/// The `on` value: starts the bridge unless it already runs. Silent when there is nothing to report;
/// never a failure: a session must start either way.
pub fn ensure_bridge() -> Option<String> {
    let bridge = live_bridge();
    let port = bridge_port(&read_env(bridge.env_dir()));
    ensure_with(&bridge, port, &mut LiveProcs, &mut || match &bridge {
        Bridge::Checkout(root) => start_bridge(root, port),
        Bridge::Installed(exe) => start_installed(exe, port),
    })
}

fn ensure_with(bridge: &Bridge, port: u16, procs: &mut dyn Procs, start: &mut dyn FnMut() -> Result<String, String>) -> Option<String> {
    if let Some(reason) = bridge.missing() {
        return Some(reason);
    }
    if let Some(pid) = procs.listener(port) {
        return match procs.info(pid) {
            Some(info) if !bridge.owns(&info) => Some(format!(
                "port {port} is held by {} (pid {pid}): set BRIDGE_PORT in {}",
                info.name,
                bridge.env_dir().join(".env").display()
            )),
            _ => None,
        };
    }
    Some(start().unwrap_or_else(|e| format!("bridge not started: {e}")))
}

fn start_bridge(root: &Path, port: u16) -> Result<String, String> {
    let log = std::env::temp_dir().join("claude-tg-bridge.log");
    prepare_dist(root, &std::env::temp_dir().join("claude-tg-bridge.build.log"))?;
    let node = on_path("node").ok_or("node is not on PATH")?;
    let stdout = File::create(&log).map_err(|e| e.to_string())?;
    let stderr = stdout.try_clone().map_err(|e| e.to_string())?;
    // Detached and windowless: the bridge outlives this launcher and never sees its Ctrl+C. An
    // absolute script path, because the stop guard identifies the bridge by its command line.
    Command::new(node)
        .arg(root.join("dist").join("index.js"))
        .current_dir(root)
        .env("BRIDGE_EXIT_ON_PORT_BUSY", "1")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(wait_for_listener(port, &log))
}

fn wait_for_listener(port: u16, log: &Path) -> String {
    for _ in 0..24 {
        std::thread::sleep(Duration::from_millis(250));
        if win::listener_pid(port).is_some() {
            return format!("bridge started on port {port}");
        }
    }
    format!("bridge did not come up within 6s, see {}", log.display())
}

/// `claude-bridge serve` без вывода: его подключает вызывающий.
fn serve_command(exe: &Path, port: u16) -> Command {
    let mut c = Command::new(exe);
    c.arg("serve")
        // У моста окружение главнее `.env`: унаследованное значение увело бы его с порта, на котором
        // лаунчер будет его ждать и останавливать.
        .env("BRIDGE_PORT", port.to_string())
        .stdin(Stdio::null())
        // Как у прежнего моста: переживает лаунчер и не видит его Ctrl+C.
        .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    // Текущий каталог лаунчера — чужой проект, а мост читает `.env` и оттуда.
    if let Some(dir) = exe.parent() {
        c.current_dir(dir);
    }
    c
}

fn start_installed(exe: &Path, port: u16) -> Result<String, String> {
    // Свой лог: прежний мост может работать рядом, и общий файл обнулился бы при запуске нового.
    let log = std::env::temp_dir().join("claude-bridge.log");
    let stdout = File::create(&log).map_err(|e| e.to_string())?;
    let stderr = stdout.try_clone().map_err(|e| e.to_string())?;
    serve_command(exe, port)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(wait_for_listener(port, &log))
}

#[cfg(test)]
mod tests;
