// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Private EPG attempt ownership and narrowly bounded crash-orphan reclamation.
//! Every new attempt holds an OS file lock until its parser finishes. A second
//! application instance therefore cannot reclaim an active attempt, even if
//! its timestamp has become old due to suspension or a long-running worker.
use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

const STALE_AFTER: Duration = Duration::from_secs(48 * 60 * 60);
const MAX_SCANNED: usize = 256;
const MAX_REMOVED: usize = 32;

pub(crate) struct AttemptSpool {
    path: PathBuf,
    lock_path: PathBuf,
    // The kernel releases this lock on abrupt process termination.
    _held_lock: File,
}

impl AttemptSpool {
    pub(crate) fn new(path: PathBuf) -> io::Result<Self> {
        let lock_path = path.with_extension("lock");
        let mut options = OpenOptions::new();
        options.write(true).read(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let held_lock = options.open(&lock_path)?;
        if let Err(error) = held_lock.try_lock() {
            let _ = fs::remove_file(&lock_path);
            return Err(io::Error::other("could not acquire private EPG stage lock"));
        }
        Ok(Self {
            path,
            lock_path,
            _held_lock: held_lock,
        })
    }
}

impl Drop for AttemptSpool {
    fn drop(&mut self) {
        // Both ordinary successful and failed refreshes leave no spool/lock.
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(&self.lock_path);
    }
}

#[derive(Default, Debug)]
pub(crate) struct SweepStats {
    pub(crate) scanned: usize,
    pub(crate) removed: usize,
}

/// One best-effort pass, only after the default private storage was validated.
/// Files created by old versions without companion locks are left alone:
/// their ownership cannot be proven against another running instance.
#[cfg(target_os = "linux")]
pub(crate) fn sweep_stale(parent: &Path, now: SystemTime) -> SweepStats {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    const O_NOFOLLOW: i32 = 0o400000;
    let mut stats = SweepStats::default();
    let Ok(directory) = fs::symlink_metadata(parent) else {
        return stats;
    };
    if !directory.is_dir() || directory.mode() & 0o077 != 0 {
        return stats;
    }
    let Ok(entries) = fs::read_dir(parent) else {
        return stats;
    };
    for entry in entries.take(MAX_SCANNED) {
        stats.scanned += 1;
        if stats.removed >= MAX_REMOVED {
            break;
        }
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = name
            .strip_prefix(".epg-")
            .and_then(|s| s.strip_suffix(".part"))
        else {
            continue;
        };
        // Exact lowercase hyphenated v4 UUIDs from Storage::epg_spool_path.
        let Ok(uuid) = uuid::Uuid::parse_str(id) else {
            continue;
        };
        if uuid.get_version_num() != 4 || uuid.to_string() != id {
            continue;
        }
        let spool_path = entry.path();
        let lock_path = parent.join(format!(".epg-{id}.lock"));
        let (Ok(spool_meta), Ok(lock_meta)) = (
            fs::symlink_metadata(&spool_path),
            fs::symlink_metadata(&lock_path),
        ) else {
            continue;
        };
        if !spool_meta.file_type().is_file()
            || !lock_meta.file_type().is_file()
            || spool_meta.uid() != directory.uid()
            || lock_meta.uid() != directory.uid()
            || spool_meta.nlink() != 1
            || lock_meta.nlink() != 1
        {
            continue;
        }
        let Ok(modified) = spool_meta.modified() else {
            continue;
        };
        if !now
            .duration_since(modified)
            .is_ok_and(|age| age >= STALE_AFTER)
        {
            continue;
        }
        // Never follow a swapped symlink, and only unlink the same inode
        // observed before opening. This directory is user-owned and 0700.
        let Ok(lock) = OpenOptions::new()
            .read(true)
            .custom_flags(O_NOFOLLOW)
            .open(&lock_path)
        else {
            continue;
        };
        let Ok(locked_meta) = lock.metadata() else {
            continue;
        };
        if locked_meta.dev() != lock_meta.dev() || locked_meta.ino() != lock_meta.ino() {
            continue;
        }
        if lock.try_lock().is_err() {
            continue;
        }
        let Ok(spool) = OpenOptions::new()
            .read(true)
            .custom_flags(O_NOFOLLOW)
            .open(&spool_path)
        else {
            continue;
        };
        let Ok(open_meta) = spool.metadata() else {
            continue;
        };
        if open_meta.dev() != spool_meta.dev()
            || open_meta.ino() != spool_meta.ino()
            || !fs::symlink_metadata(&spool_path).is_ok_and(|m| {
                m.dev() == open_meta.dev() && m.ino() == open_meta.ino() && m.file_type().is_file()
            })
            || !fs::symlink_metadata(&lock_path).is_ok_and(|m| {
                m.dev() == locked_meta.dev()
                    && m.ino() == locked_meta.ino()
                    && m.file_type().is_file()
            })
        {
            continue;
        }
        drop(spool);
        // Failure to remove the stage leaves the lock marker for a later pass.
        if fs::remove_file(&spool_path).is_ok() {
            stats.removed += 1;
            let _ = fs::remove_file(&lock_path);
        }
    }
    stats
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn sweep_stale(_parent: &Path, _now: SystemTime) -> SweepStats {
    // No verified no-follow deletion implementation for this platform.
    SweepStats::default()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    struct PrivateDir(PathBuf);
    impl PrivateDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("ops-spool-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn stage(&self, old: bool) -> PathBuf {
            let path = self.0.join(format!(".epg-{}.part", uuid::Uuid::new_v4()));
            fs::write(&path, b"synthetic").unwrap();
            if old {
                let last_week = SystemTime::now() - Duration::from_secs(7 * 86400);
                File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(last_week))
                    .unwrap();
            }
            path
        }
    }
    impl Drop for PrivateDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn orphan(dir: &PrivateDir, old: bool) -> PathBuf {
        let path = dir.stage(old);
        let lock = path.with_extension("lock");
        // Simulate SIGKILL: lock marker persists but kernel lock is released.
        File::create(lock).unwrap();
        path
    }
    #[test]
    fn orphan_restart_and_idempotence() {
        let dir = PrivateDir::new();
        let stale = orphan(&dir, true);
        let recent = orphan(&dir, false);
        let now = SystemTime::now();
        assert_eq!(sweep_stale(&dir.0, now).removed, 1);
        assert!(!stale.exists());
        assert!(!stale.with_extension("lock").exists());
        assert!(recent.exists());
        assert_eq!(sweep_stale(&dir.0, now).removed, 0);
    }
    #[test]
    fn rejects_live_locks_symlinks_directories_malformed_names_and_other_files() {
        let dir = PrivateDir::new();
        let live = dir.stage(true);
        let lock = live.with_extension("lock");
        let holder = File::create(&lock).unwrap();
        holder.try_lock().unwrap();
        let unrelated = dir.0.join("user-guide.xml");
        fs::write(&unrelated, b"keep").unwrap();
        let malformed = dir.0.join(".epg-not-a-uuid.part");
        fs::write(&malformed, b"keep").unwrap();
        let symlink_spool = dir.stage(true);
        fs::remove_file(&symlink_spool).unwrap();
        symlink(&unrelated, &symlink_spool).unwrap();
        File::create(symlink_spool.with_extension("lock")).unwrap();
        let symlink_lock = dir.stage(true);
        symlink(&unrelated, symlink_lock.with_extension("lock")).unwrap();
        let directory = dir.stage(true);
        fs::remove_file(&directory).unwrap();
        fs::create_dir(&directory).unwrap();
        File::create(directory.with_extension("lock")).unwrap();
        let legacy = dir.stage(true); // old-format stage has no lock marker
        assert_eq!(sweep_stale(&dir.0, SystemTime::now()).removed, 0);
        for path in [
            &live,
            &unrelated,
            &malformed,
            &symlink_spool,
            &symlink_lock,
            &directory,
            &legacy,
        ] {
            assert!(path.exists());
        }
    }
    #[test]
    fn bounded_enumeration_and_cross_install_isolation() {
        let first = PrivateDir::new();
        let second = PrivateDir::new();
        let other = orphan(&second, true);
        for n in 0..300 {
            fs::write(first.0.join(format!("ignored-{n:04}")), b"x").unwrap();
        }
        let counts = sweep_stale(&first.0, SystemTime::now());
        assert!(counts.scanned <= MAX_SCANNED);
        assert_eq!(counts.removed, 0);
        assert!(other.exists());
    }
    #[test]
    fn ordinary_success_and_failure_drop_both_artifacts() {
        let dir = PrivateDir::new();
        for _ in 0..2 {
            let path = dir.0.join(format!(".epg-{}.part", uuid::Uuid::new_v4()));
            let guard = AttemptSpool::new(path.clone()).unwrap();
            fs::write(&path, b"partial-or-complete").unwrap();
            assert!(path.with_extension("lock").exists());
            drop(guard);
            assert!(!path.exists());
            assert!(!path.with_extension("lock").exists());
        }
    }
}
