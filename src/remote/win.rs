//! Win32 for the bridge path: who listens on a TCP port (IPv4 and IPv6) and a process's command line.
#![expect(unsafe_code, reason = "ntdll and process FFI")]

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt as _;
use std::path::PathBuf;

use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

use crate::sys::{Family, tcp_listeners};

struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: a handle OpenProcess returned, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

fn listener_in(family: Family, port: u16) -> Option<u32> {
    tcp_listeners(family).into_iter().find(|l| l.port == port).map(|l| l.pid)
}

/// `Get-NetTCPConnection -LocalPort <port> -State Listen`: the owning pid of a listener on any
/// address of either family (node listens on `::` by default).
pub(crate) fn listener_pid(port: u16) -> Option<u32> {
    listener_in(Family::V4, port).or_else(|| listener_in(Family::V6, port))
}

/// Полный путь исполняемого файла процесса, `None`, когда его не открыть или не прочитать.
pub(crate) fn image_path(pid: u32) -> Option<PathBuf> {
    // SAFETY: plain call; a null handle is checked below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let h = Owned(h);
    // Предел длинного пути Windows: короткий буфер дал бы `None` и `off` не тронул бы настоящий мост.
    let mut buf = vec![0u16; 32_768];
    let mut len = u32::try_from(buf.len()).ok()?;
    // SAFETY: `buf` holds `len` writable u16; the call writes at most that and updates `len`.
    let ok = unsafe { QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &raw mut len) };
    if ok == 0 {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(buf.get(..len as usize)?)))
}

/// A process's command line, `None` when it cannot be opened or read.
pub(crate) fn command_line(pid: u32) -> Option<String> {
    // SAFETY: plain call; a null handle is checked below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let h = Owned(h);
    let mut len = 0u32;
    // SAFETY: a zero-length probe for the size; the status is ignored, `len` is what is read.
    unsafe { NtQueryInformationProcess(h.0, ProcessCommandLineInformation, std::ptr::null_mut(), 0, &raw mut len) };
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    let cap = u32::try_from(buf.len() * 8).ok()?;
    // SAFETY: `buf` has `cap` writable, 8-aligned bytes for the UNICODE_STRING and its text.
    let st = unsafe { NtQueryInformationProcess(h.0, ProcessCommandLineInformation, buf.as_mut_ptr().cast(), cap, &raw mut len) };
    if st < 0 {
        return None;
    }
    let base = buf.as_ptr().addr();
    let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_ne_bytes()).collect();
    // UNICODE_STRING: Length (u16) at 0, Buffer (pointer) at 8, pointing into this same buffer.
    let chars = usize::from(u16::from_ne_bytes(bytes.get(0..2)?.try_into().ok()?));
    let ptr = usize::try_from(u64::from_ne_bytes(bytes.get(8..16)?.try_into().ok()?)).ok()?;
    let off = ptr.checked_sub(base)?;
    let text: Vec<u16> = bytes
        .get(off..off + chars)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_ne_bytes(*c))
        .collect();
    Some(String::from_utf16_lossy(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loopback_listener_is_found_with_this_pid_in_both_families() {
        let v4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p4 = v4.local_addr().unwrap().port();
        assert_eq!(listener_pid(p4), Some(std::process::id()));
        let v6 = std::net::TcpListener::bind("[::1]:0").unwrap();
        let p6 = v6.local_addr().unwrap().port();
        assert_eq!(listener_pid(p6), Some(std::process::id()));
        drop((v4, v6));
        assert_eq!(listener_pid(p4), None, "a closed port has no listener");
    }

    /// Убивает пробу и при упавшей проверке: иначе она пережила бы тест.
    struct Probe(std::process::Child);

    impl Drop for Probe {
        fn drop(&mut self) {
            drop(self.0.kill());
            drop(self.0.wait());
        }
    }

    #[test]
    fn the_image_path_is_the_real_file_and_a_dead_pid_has_none() {
        assert_eq!(image_path(std::process::id()), Some(std::env::current_exe().unwrap()));
        assert_eq!(image_path(u32::MAX - 3), None);
    }

    // `claude-bridge setup` переименовывает занятый exe, и `off` обязан найти идущий мост и после
    // этого. Поэтому настоящий процесс, а не предположение о поведении Windows.
    #[test]
    fn the_image_path_of_a_running_process_follows_a_rename() {
        let dir = std::env::temp_dir().join(format!("claude-auto-rs-image-{}", crate::log::new_run_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let (first, second) = (dir.join("probe.exe"), dir.join("probe.exe.old-0"));
        std::fs::copy(PathBuf::from(system).join("System32").join("PING.EXE"), &first).unwrap();
        let probe = Probe(
            std::process::Command::new(&first)
                .args(["-n", "30", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let pid = probe.0.id();
        assert_eq!(image_path(pid), Some(first.clone()));
        std::fs::rename(&first, &second).unwrap();
        assert_eq!(image_path(pid), Some(second));
        drop(probe);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn own_command_line_names_this_test_binary() {
        let exe = std::env::current_exe().unwrap();
        let stem = exe.file_stem().unwrap().to_string_lossy().into_owned();
        assert!(command_line(std::process::id()).unwrap().contains(&stem));
        assert_eq!(command_line(u32::MAX - 3), None);
    }
}
