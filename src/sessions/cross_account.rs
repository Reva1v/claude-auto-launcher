//! (local) The session picker over every account's store: with `sharing` off a session may live in
//! another profile, and resuming it there is a file copy into the launching profile's own store.
//! Kept apart from `listing.rs` so a pull from upstream stays a small merge.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::listing::{Session, session_files};

/// Another account's projects store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherStore {
    pub account: String,
    pub root: PathBuf,
}

/// A picked session that lives in another account's store, copied before claude starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub session_id: String,
    pub source: PathBuf,
    pub from_account: String,
}

fn modified(p: &Path) -> SystemTime {
    std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH)
}

/// `Join-SessionSnapshot`: the launching account's snapshot plus every other store's, newest first,
/// one row per session id: a session transferred once exists twice, the newer copy wins.
pub fn joined_snapshot(primary: Vec<PathBuf>, others: &[OtherStore], slugs: &[String]) -> Vec<PathBuf> {
    let mut all: Vec<(PathBuf, SystemTime)> = primary
        .into_iter()
        .chain(others.iter().flat_map(|o| session_files(&o.root, slugs)))
        .map(|p| {
            let t = modified(&p);
            (p, t)
        })
        .collect();
    all.sort_by_key(|x| std::cmp::Reverse(x.1));
    let mut seen = HashSet::new();
    all.into_iter()
        .filter(|(p, _)| seen.insert(p.file_name().map(|n| n.to_string_lossy().to_lowercase())))
        .map(|(p, _)| p)
        .collect()
}

/// `Add-SessionAccountTag`: names the other account whose store holds this session; a session of
/// the launching account keeps an empty tag.
pub fn tag_sessions(sessions: &mut [Session], others: &[OtherStore]) {
    for s in sessions {
        s.account = others
            .iter()
            .find(|o| s.path.starts_with(&o.root))
            .map_or_else(String::new, |o| o.account.clone());
    }
}

/// The transfer a pick needs, when the session lives in another account's store.
pub fn transfer_of(s: &Session) -> Option<Transfer> {
    (!s.account.is_empty()).then(|| Transfer {
        session_id: s.session_id.clone(),
        source: s.path.clone(),
        from_account: s.account.clone(),
    })
}

/// `Copy-SessionToProfile`: the transcript into the target store, slug directory preserved (the
/// slug is how claude maps a transcript back to its directory). An up-to-date target is never
/// touched; an older one is renamed to a `.bak` sibling, which the `*.jsonl` scan never offers.
pub fn copy_session_to_profile(source: &Path, target_root: &Path) -> std::io::Result<PathBuf> {
    let missing = || std::io::Error::new(std::io::ErrorKind::InvalidInput, "transcript path has no slug directory");
    let slug = source.parent().and_then(Path::file_name).ok_or_else(missing)?;
    let name = source.file_name().ok_or_else(missing)?;
    let dir = target_root.join(slug);
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(name);
    if target.exists() {
        if modified(&target) >= modified(source) {
            return Ok(target);
        }
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        std::fs::rename(&target, format!("{}.{stamp}.bak", target.display()))?;
    }
    std::fs::copy(source, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            drop(std::fs::remove_dir_all(&self.0));
        }
    }
    fn tmp(name: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("cars-cross-{name}-{}", crate::log::new_run_id()));
        std::fs::create_dir_all(&d).unwrap();
        Tmp(d)
    }
    fn transcript(root: &Path, slug: &str, id: &str, age_s: u64, body: &str) -> PathBuf {
        let d = root.join(slug);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(format!("{id}.jsonl"));
        std::fs::write(&p, body).unwrap();
        let t = SystemTime::now().checked_sub(Duration::from_secs(age_s)).unwrap();
        std::fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
        p
    }

    #[test]
    fn the_snapshot_folds_both_stores_newest_first_and_the_newer_copy_wins() {
        let t = tmp("snap");
        let (a, b) = (t.0.join("a"), t.0.join("b"));
        let own = transcript(&a, "S", "own", 100, "x");
        let dup_old = transcript(&a, "S", "dup", 300, "x");
        let dup_new = transcript(&b, "S", "dup", 50, "x");
        let foreign = transcript(&b, "S", "foreign", 200, "x");
        transcript(&b, "OTHER", "skipped", 1, "x");
        let others = [OtherStore {
            account: "b".into(),
            root: b,
        }];
        let slugs = ["S".to_owned()];
        let got = joined_snapshot(session_files(&a, &slugs), &others, &slugs);
        assert_eq!(got, [dup_new, own, foreign]);
        assert!(!got.contains(&dup_old), "the superseded copy is never offered");
    }

    #[test]
    fn only_a_foreign_session_is_tagged_and_transferred() {
        let others = [OtherStore {
            account: "mamoru".into(),
            root: PathBuf::from(r"C:\m\projects"),
        }];
        let mut rows = [
            Session {
                session_id: "f".into(),
                path: PathBuf::from(r"C:\m\projects\S\f.jsonl"),
                ..Session::default()
            },
            Session {
                session_id: "o".into(),
                path: PathBuf::from(r"C:\o\projects\S\o.jsonl"),
                account: "stale".into(),
                ..Session::default()
            },
        ];
        tag_sessions(&mut rows, &others);
        assert_eq!(rows[0].account, "mamoru");
        assert_eq!(rows[1].account, "", "the launching account's row has no tag");
        assert_eq!(transfer_of(&rows[0]).unwrap().from_account, "mamoru");
        assert_eq!(transfer_of(&rows[1]), None);
    }

    #[test]
    fn a_copy_keeps_the_slug_and_never_destroys_a_target() {
        let t = tmp("copy");
        let (src_root, dst_root) = (t.0.join("src"), t.0.join("dst"));
        let src = transcript(&src_root, "S", "id", 100, "source");
        let dst = copy_session_to_profile(&src, &dst_root).unwrap();
        assert_eq!(dst, dst_root.join("S").join("id.jsonl"));
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "source");

        let newer = transcript(&dst_root, "S", "id", 10, "continued here");
        copy_session_to_profile(&src, &dst_root).unwrap();
        assert_eq!(std::fs::read_to_string(&newer).unwrap(), "continued here", "up to date: untouched");

        let src = transcript(&src_root, "S", "id", 1, "source v2");
        copy_session_to_profile(&src, &dst_root).unwrap();
        assert_eq!(std::fs::read_to_string(&newer).unwrap(), "source v2");
        let baks: Vec<String> = std::fs::read_dir(dst_root.join("S"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| Path::new(n).extension().is_some_and(|x| x.eq_ignore_ascii_case("bak")))
            .collect();
        assert_eq!(baks.len(), 1, "the older target is kept as .bak");
        assert_eq!(
            std::fs::read_to_string(dst_root.join("S").join(&baks[0])).unwrap(),
            "continued here"
        );
    }
}
