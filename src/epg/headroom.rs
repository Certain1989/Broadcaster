// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Best-effort EPG filesystem admission; never treats a missing probe as ENOSPC.
//! No background polling, disk allocations, temp-directory guesses or new quotas.
use adw::{gio, prelude::*};
use std::path::{Path, PathBuf};

pub(crate) const MIB: u64 = 1024 * 1024;
pub(crate) const DOWNLOAD_FLOOR: u64 = 256 * MIB;
pub(crate) const IMPORT_FLOOR: u64 = 128 * MIB;
pub(crate) const DOWNLOAD_CHECK_BYTES: u64 = 16 * MIB;
const MAX_VARIABLE_RESERVE: u64 = 6 * 1024 * MIB;

#[derive(Clone, Debug)]
struct Space {
    free: u64,
    id: Option<String>,
}

/// Query the parent directory because a freshly generated spool need not exist.
/// GIO may not report filesystem attributes inside a portal or sandbox.
#[cfg(test)]
std::thread_local! {
    static TEST_SPACE: std::cell::RefCell<Option<(PathBuf, std::collections::VecDeque<Option<u64>>)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct TestSpaceOverride;
#[cfg(test)]
impl Drop for TestSpaceOverride {
    fn drop(&mut self) {
        TEST_SPACE.with(|slot| *slot.borrow_mut() = None);
    }
}
#[cfg(test)]
pub(crate) fn override_space(path: PathBuf, readings: Vec<Option<u64>>) -> TestSpaceOverride {
    TEST_SPACE.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "one active probe override per test"
        );
        *slot.borrow_mut() = Some((path, readings.into()));
    });
    TestSpaceOverride
}

fn space(path: &Path) -> Option<Space> {
    #[cfg(test)]
    if let Some(reading) = TEST_SPACE.with(|slot| {
        let mut borrowed = slot.borrow_mut();
        borrowed.as_mut().and_then(|(target, queue)| {
            (target == path).then(|| {
                queue
                    .pop_front()
                    .expect("all expected mocked filesystem checks must be enumerated")
            })
        })
    }) {
        return reading.map(|free| Space {
            free,
            id: Some("mock-db-mount".to_owned()),
        });
    }
    let directory = path.parent().unwrap_or(path);
    let info = gio::File::for_path(directory)
        .query_filesystem_info("filesystem::free,filesystem::id", gio::Cancellable::NONE)
        .ok()?;
    if !info.has_attribute("filesystem::free") {
        return None;
    }
    Some(Space {
        free: info.attribute_uint64("filesystem::free"),
        id: info
            .attribute_string("filesystem::id")
            .map(|id| id.to_string()),
    })
}

fn sufficient(observed: Option<&Space>, needed: u64) -> bool {
    observed.is_none_or(|sample| sample.free >= needed)
}

fn distinct_mounts(a: Option<&Space>, b: Option<&Space>) -> bool {
    matches!((a.and_then(|s| s.id.as_ref()), b.and_then(|s| s.id.as_ref())),
        (Some(left), Some(right)) if left != right)
}

fn mount_policy_allows(db: Option<&Space>, staged: Option<&Space>, required: u64) -> bool {
    sufficient(db, required) && (!distinct_mounts(db, staged) || sufficient(staged, IMPORT_FLOOR))
}

pub(crate) fn download_allowed(path: &Path) -> bool {
    sufficient(space(path).as_ref(), DOWNLOAD_FLOOR)
}

/// S is the actual staged file length, never a Content-Length or URL hint.
/// A capped multiplication avoids overflows even for pathological test inputs.
pub(crate) fn import_reserve(staged_bytes: u64, gzip: bool) -> u64 {
    let multiplier = if gzip { 24 } else { 4 };
    DOWNLOAD_FLOOR
        + staged_bytes
            .saturating_mul(multiplier)
            .min(MAX_VARIABLE_RESERVE)
}

#[derive(Debug)]
pub(crate) struct ImportHeadroom {
    database: PathBuf,
    staged: PathBuf,
    reserve: u64,
}
impl ImportHeadroom {
    pub(crate) fn new(database: PathBuf, staged: PathBuf, staged_size: u64, gzip: bool) -> Self {
        Self {
            database,
            staged,
            reserve: import_reserve(staged_size, gzip),
        }
    }

    /// Staging has already consumed blocks. Do not subtract its length again
    /// or apply the DB growth multiplier to a separate staging filesystem.
    pub(crate) fn admit(&self) -> bool {
        let db = space(&self.database);
        let staged = space(&self.staged);
        mount_policy_allows(db.as_ref(), staged.as_ref(), self.reserve)
    }

    /// The DB/WAL floor is checked every 4096 parsed programmes and once
    /// immediately before COMMIT. A *known separate* staging mount is checked
    /// only against the approved active floor, never against R.
    pub(crate) fn active(&self) -> bool {
        let db = space(&self.database);
        let staged = space(&self.staged);
        mount_policy_allows(db.as_ref(), staged.as_ref(), IMPORT_FLOOR)
    }
}

/// The SQLite TEMP path cannot be inferred reliably from SQLite's defaults.
/// No path is invented and no TEMP location or memory policy is overridden.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_reserve_and_overflow() {
        assert_eq!(import_reserve(0, false), 256 * MIB);
        assert_eq!(import_reserve(MIB, false), 260 * MIB);
        assert_eq!(import_reserve(MIB, true), 280 * MIB);
        assert_eq!(import_reserve(u64::MAX, true), (6 * 1024 + 256) * MIB);
        assert_eq!(import_reserve(u64::MAX, false), (6 * 1024 + 256) * MIB);
        assert_eq!(import_reserve(256 * MIB, true), (6 * 1024 + 256) * MIB);
        assert_eq!(import_reserve(1536 * MIB, false), (6 * 1024 + 256) * MIB);
    }

    #[test]
    fn availability_boundaries_and_missing_probe_fail_open() {
        let required = DOWNLOAD_FLOOR;
        let sample = |free| Space { free, id: None };
        assert!(sufficient(None, required));
        assert!(!sufficient(Some(&sample(required - 1)), required));
        assert!(sufficient(Some(&sample(required)), required));
        assert!(sufficient(Some(&sample(required + 1)), required));
        assert!(!sufficient(Some(&sample(IMPORT_FLOOR - 1)), IMPORT_FLOOR));
        assert!(sufficient(Some(&sample(IMPORT_FLOOR)), IMPORT_FLOOR));
    }

    #[test]
    fn separated_mount_admission_uses_only_approved_floors() {
        let db = Space {
            free: 1024 * MIB,
            id: Some("db".to_owned()),
        };
        let staged = Space {
            free: IMPORT_FLOOR - 1,
            id: Some("stage".to_owned()),
        };
        let required = 1024 * MIB;
        assert!(!mount_policy_allows(Some(&db), Some(&staged), required));
        let stage_ok = Space {
            free: IMPORT_FLOOR,
            ..staged
        };
        assert!(mount_policy_allows(Some(&db), Some(&stage_ok), required));
        let shared = Space {
            id: Some("db".to_owned()),
            free: 0,
        };
        assert!(mount_policy_allows(Some(&db), Some(&shared), required));
        assert!(mount_policy_allows(None, None, required));
        assert!(mount_policy_allows(None, Some(&stage_ok), required));
    }

    #[test]
    fn only_confidently_different_mounts_are_separate() {
        let sample = |id: Option<&str>| Space {
            free: 512 * MIB,
            id: id.map(str::to_owned),
        };
        assert!(!distinct_mounts(
            Some(&sample(Some("a"))),
            Some(&sample(Some("a")))
        ));
        assert!(distinct_mounts(
            Some(&sample(Some("a"))),
            Some(&sample(Some("b")))
        ));
        assert!(!distinct_mounts(
            Some(&sample(None)),
            Some(&sample(Some("b")))
        ));
        assert!(!distinct_mounts(None, Some(&sample(Some("b")))));
    }
}
