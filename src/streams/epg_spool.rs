// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Private EPG attempt ownership and narrowly bounded crash-orphan reclamation.
//! Each worker retains an OS file lock until it finishes. A second
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
        if held_lock.try_lock().is_err() {
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
    sweep_stale_with_hook(parent, now, |_, _| {})
}

// A test-only substitution point immediately before the last path validation.
// The production call has a zero-cost no-op hook. Same-UID hostile leaf writers
// can still race the final check and unlink; see the documented threat boundary.
#[cfg(target_os = "linux")]
fn sweep_stale_with_hook<F: FnMut(&Path, &Path)>(
    parent: &Path,
    now: SystemTime,
    before_unlink: F,
) -> SweepStats {
    sweep_stale_with_ops(parent, now, before_unlink, |path| fs::remove_file(path))
}

#[cfg(target_os = "linux")]
fn sweep_stale_with_ops<F, U>(
    parent: &Path,
    now: SystemTime,
    mut before_unlink: F,
    mut unlink: U,
) -> SweepStats
where
    F: FnMut(&Path, &Path),
    U: FnMut(&Path) -> io::Result<()>,
{
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    // Linux open(2) flags differ between x86-64 and AArch64.
    // Use target-specific libc definitions; hardcoded x86 values fail closed
    // on AArch64 (and silently disable orphan recovery).
    let mut stats = SweepStats::default();
    // Keep the verified directory inode open through enumeration AND unlink.
    // /proc/self/fd is Linux-only; if procfs is not accessible, fail closed.
    let Ok(parent_fd) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(parent)
    else {
        return stats;
    };
    let Ok(directory) = parent_fd.metadata() else {
        return stats;
    };
    let anchored_parent = PathBuf::from(format!("/proc/self/fd/{}", parent_fd.as_raw_fd()));
    if !directory.is_dir() || directory.mode() & 0o077 != 0 {
        return stats;
    }
    let Ok(entries) = fs::read_dir(&anchored_parent) else {
        return stats;
    };
    // Best-effort bound: iterator order is not fair. With >256 permanent
    // unrelated entries, some orphans can remain indefinitely; never widen
    // work or delete unknown files just to guarantee collection.
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
        let lock_path = anchored_parent.join(format!(".epg-{id}.lock"));
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
        // No-follow open and inode checks reject substituted leaves observed
        // before deletion. The private 0700 parent is held by a directory fd;
        // a concurrent ancestor rename cannot redirect the unlink.
        let Ok(lock) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
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
            .custom_flags(libc::O_NOFOLLOW)
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
        // Deterministically exercise last-moment replacement in tests. There
        // is no atomic compare-and-unlink for a hostile same-UID leaf writer;
        // this extra check narrows, but cannot eliminate, that race.
        before_unlink(&spool_path, &lock_path);
        if !fs::symlink_metadata(&spool_path).is_ok_and(|m| {
            m.dev() == open_meta.dev()
                && m.ino() == open_meta.ino()
                && m.file_type().is_file()
                && m.nlink() == 1
        }) || !fs::symlink_metadata(&lock_path).is_ok_and(|m| {
            m.dev() == locked_meta.dev()
                && m.ino() == locked_meta.ino()
                && m.file_type().is_file()
                && m.nlink() == 1
        }) {
            continue;
        }
        drop(spool);
        // Failure to remove the stage leaves the lock marker for a later pass.
        if unlink(&spool_path).is_ok() {
            stats.removed += 1;
            let _ = unlink(&lock_path);
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
    fn native_directory_open_flags_enforce_nofollow_and_directory_only() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;

        let dir = PrivateDir::new();
        let opened = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(&dir.0)
            .expect("native Linux flags must open a private directory");
        let anchored = format!("/proc/self/fd/{}", opened.as_raw_fd());
        fs::read_dir(anchored).expect("the opened directory must be enumerable by fd");

        let symlink_parent = dir.0.with_extension("symlink");
        symlink(&dir.0, &symlink_parent).unwrap();
        assert!(OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(&symlink_parent)
            .is_err());
        let ordinary_file = dir.0.with_extension("ordinary");
        fs::write(&ordinary_file, b"not a directory").unwrap();
        assert!(OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(&ordinary_file)
            .is_err());
        fs::remove_file(symlink_parent).unwrap();
        fs::remove_file(ordinary_file).unwrap();
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
    fn hardlinks_and_unsafe_directory_are_not_reclaimed() {
        let dir = PrivateDir::new();
        let linked = orphan(&dir, true);
        let other = dir.0.join("other-user-file");
        fs::hard_link(&linked, &other).unwrap();
        assert_eq!(sweep_stale(&dir.0, SystemTime::now()).removed, 0);
        assert!(linked.exists());
        assert!(other.exists());
        fs::remove_file(&other).unwrap();
        // Even a candidate with a released lock must remain untouched if
        // its containing directory is no longer private.
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(sweep_stale(&dir.0, SystemTime::now()).removed, 0);
        assert!(linked.exists());
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(sweep_stale(&dir.0, SystemTime::now()).removed, 1);
        assert!(!linked.exists());
    }

    #[test]
    fn removal_budget_allows_progress_on_repeated_launches() {
        let dir = PrivateDir::new();
        let stages: Vec<_> = (0..MAX_REMOVED + 8).map(|_| orphan(&dir, true)).collect();
        let now = SystemTime::now();
        let first = sweep_stale(&dir.0, now);
        assert!(first.scanned <= MAX_SCANNED);
        assert_eq!(first.removed, MAX_REMOVED);
        assert_eq!(stages.iter().filter(|stage| stage.exists()).count(), 8);
        assert_eq!(sweep_stale(&dir.0, now).removed, 8);
        assert!(stages.iter().all(|stage| !stage.exists()));
    }

    #[test]
    fn leaf_and_lock_substitution_before_unlink_are_rejected() {
        for replace_lock in [false, true] {
            let dir = PrivateDir::new();
            let old = orphan(&dir, true);
            let marker = old.with_extension("lock");
            let moved = dir.0.join("old-entry");
            let target = if replace_lock {
                marker.clone()
            } else {
                old.clone()
            };
            let mut hit = false;
            let result = sweep_stale_with_hook(&dir.0, SystemTime::now(), |spool, lock| {
                hit = true;
                let active = if replace_lock { lock } else { spool };
                fs::rename(active, &moved).unwrap();
                fs::write(active, b"new and unrelated").unwrap();
            });
            assert!(hit);
            assert_eq!(result.removed, 0);
            assert_eq!(fs::read(&target).unwrap(), b"new and unrelated");
            assert!(moved.exists());
        }
    }

    #[test]
    fn ancestor_substitution_does_not_redirect_directory_fd_unlink() {
        let dir = PrivateDir::new();
        let old = orphan(&dir, true);
        let filename = old.file_name().unwrap().to_owned();
        let relocated = dir.0.with_extension("relocated");
        let replacement = dir.0.join(&filename);
        let mut hit = false;
        let result = sweep_stale_with_hook(&dir.0, SystemTime::now(), |_, _| {
            hit = true;
            fs::rename(&dir.0, &relocated).unwrap();
            fs::create_dir(&dir.0).unwrap();
            fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(&replacement, b"must survive").unwrap();
        });
        assert!(hit);
        assert_eq!(result.removed, 1);
        assert_eq!(fs::read(&replacement).unwrap(), b"must survive");
        assert!(!relocated.join(&filename).exists());
        fs::remove_dir_all(relocated).unwrap();
    }

    // Every synchronization boundary in this test is timed: a failed-closed
    // scanner or a stuck contender must produce a test failure, not a hung CI.
    #[test]
    fn concurrent_sweepers_respect_a_busy_lock_and_remove_once() {
        use std::sync::mpsc;
        use std::thread;

        const RENDEZVOUS_LIMIT: Duration = Duration::from_secs(20);
        let dir = PrivateDir::new();
        let stage = orphan(&dir, true);
        let (arrived_tx, arrived_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (first_tx, first_rx) = mpsc::channel();
        let parent = dir.0.clone();
        let now = SystemTime::now();

        let first = thread::spawn(move || {
            let result = sweep_stale_with_hook(&parent, now, |_, _| {
                let _ = arrived_tx.send(());
                // The main thread can spend one deadline waiting for arrival
                // and another waiting for the contender before releasing us.
                release_rx
                    .recv_timeout(RENDEZVOUS_LIMIT * 3)
                    .expect("first sweeper was not released within the deadline");
            });
            let _ = first_tx.send(result);
        });

        // An early fail-closed return closes the arrival channel. Do not
        // assume the hook is always called or wait on an unbounded barrier.
        let arrival = arrived_rx.recv_timeout(RENDEZVOUS_LIMIT);
        let mut contender = None;
        let mut contender_result = None;
        let mut stage_held = false;
        if arrival.is_ok() {
            stage_held = stage.exists();
            let (second_tx, second_rx) = mpsc::channel();
            let second_parent = dir.0.clone();
            contender = Some(thread::spawn(move || {
                let _ = second_tx.send(sweep_stale(&second_parent, now));
            }));
            contender_result = Some(second_rx.recv_timeout(RENDEZVOUS_LIMIT));
        }

        // Release the first worker *before* any assertion, even if the
        // contender times out or the scanner never reached the hook.
        let _ = release_tx.send(());
        let first_result = first_rx.recv_timeout(RENDEZVOUS_LIMIT);
        // Join only completed threads: an indefinite worker must never turn
        // a timeout assertion into a second unbounded wait.
        if !matches!(first_result, Err(mpsc::RecvTimeoutError::Timeout)) {
            // A delivered result or a disconnected sender guarantees the
            // worker cannot still be blocked in its hook.
            first.join().expect("first sweeper panicked");
        }
        if let Some(second) = contender {
            if !matches!(contender_result, Some(Err(mpsc::RecvTimeoutError::Timeout))) {
                second.join().expect("contending sweeper panicked");
            }
        }

        assert!(
            arrival.is_ok(),
            "first sweeper never reached before_unlink (arrival={arrival:?}, result={first_result:?}); investigate scanner/lock fail-closed paths"
        );
        assert!(
            stage_held,
            "active spool disappeared while first lock was held"
        );
        assert_eq!(
            contender_result
                .expect("second sweeper not started")
                .expect("contending sweep blocked or failed to complete")
                .removed,
            0,
            "contending sweeper must not remove a locked spool"
        );
        assert_eq!(
            first_result
                .expect("first sweeper blocked or failed to complete")
                .removed,
            1,
            "first sweeper must remove the orphan exactly once"
        );
        assert!(!stage.exists());
    }

    #[test]
    fn scanner_without_a_companion_lock_fails_closed_without_entering_unlink_hook() {
        use std::sync::mpsc;
        use std::thread;

        let dir = PrivateDir::new();
        let lockless = dir.stage(true);
        let (hook_tx, hook_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let parent = dir.0.clone();
        let worker = thread::spawn(move || {
            let stats = sweep_stale_with_hook(&parent, SystemTime::now(), |_, _| {
                let _ = hook_tx.send(());
            });
            let _ = done_tx.send(stats);
        });
        let result = done_rx.recv_timeout(Duration::from_secs(20));
        if !matches!(result, Err(mpsc::RecvTimeoutError::Timeout)) {
            worker.join().expect("lockless sweep panicked");
        }
        assert_eq!(result.expect("lockless sweep stalled").removed, 0);
        assert!(matches!(
            hook_rx.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert!(lockless.exists());
    }

    #[test]
    fn future_mtime_and_missing_lock_fail_closed() {
        let dir = PrivateDir::new();
        let future = orphan(&dir, false);
        let tomorrow = SystemTime::now() + Duration::from_secs(86400);
        File::options()
            .write(true)
            .open(&future)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(tomorrow))
            .unwrap();
        let lockless = dir.stage(true);
        assert_eq!(sweep_stale(&dir.0, SystemTime::now()).removed, 0);
        assert!(future.exists());
        assert!(lockless.exists());
    }

    #[test]
    fn injected_unlink_denial_keeps_spool_and_lock_for_retry() {
        let dir = PrivateDir::new();
        let stage = orphan(&dir, true);
        let lock = stage.with_extension("lock");
        let now = SystemTime::now();
        let stats = sweep_stale_with_ops(
            &dir.0,
            now,
            |_, _| {},
            |_| Err(io::Error::from(io::ErrorKind::PermissionDenied)),
        );
        assert_eq!(stats.removed, 0);
        assert!(stage.exists());
        assert!(lock.exists());
        assert_eq!(sweep_stale(&dir.0, now).removed, 1);
    }

    #[test]
    fn injected_lock_unlink_failure_leaves_conservative_marker() {
        let dir = PrivateDir::new();
        let stage = orphan(&dir, true);
        let lock = stage.with_extension("lock");
        let stats = sweep_stale_with_ops(
            &dir.0,
            SystemTime::now(),
            |_, _| {},
            |path| {
                if path.extension() == Some(std::ffi::OsStr::new("lock")) {
                    Err(io::Error::from(io::ErrorKind::PermissionDenied))
                } else {
                    fs::remove_file(path)
                }
            },
        );
        assert_eq!(stats.removed, 1);
        assert!(!stage.exists());
        assert!(lock.exists());
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
