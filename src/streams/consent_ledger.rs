// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Durable revocation tombstones for saved playlist LAN grants. A denied key
//! takes precedence over GSettings after process restart. The SQLite database
//! is not modified; importing a database therefore cannot import permissions.
//! No playlist locator or user-provided URL is written to this file.
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use super::storage::{self, Storage};

const FILENAME: &str = ".playlist-lan-revocations-v1";
const HEADER: &str = "BROADCASTER_LAN_REVOCATIONS_V1\n";
const MAX_KEYS: usize = 20_000;
const MAX_BYTES: u64 = 1_400_000;

pub(crate) fn path() -> PathBuf {
    Storage::new().database_path().with_file_name(FILENAME)
}

/// Missing ledger is the initial state, but unreadable, corrupt or insecure
/// state must deny access instead of resurrecting an old GSettings grant.
pub(crate) fn is_revoked(path: &Path, token: &str) -> io::Result<bool> {
    Ok(read(path)?.contains(token))
}

fn read(path: &Path) -> io::Result<HashSet<String>> {
    let parent = path.parent().ok_or_else(invalid)?;
    // Refuse unexpected path types, foreign ownership and an insecure parent.
    // Existing source databases are already required to live in a 0700 dir.
    match fs::symlink_metadata(parent) {
        Ok(meta) => {
            if !meta.file_type().is_dir()
                || meta.permissions().mode() & 0o077 != 0
                || meta.uid() != unsafe { libc::geteuid() }
            {
                return Err(invalid());
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(e) => return Err(e),
    }
    let mut opts = OpenOptions::new();
    opts.read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = match opts.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.permissions().mode() & 0o077 != 0
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.len() > MAX_BYTES
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
    let rest = text.strip_prefix(HEADER).ok_or_else(invalid)?;
    if !rest.is_empty() && !rest.ends_with('\n') {
        return Err(invalid());
    }
    let mut entries = HashSet::new();
    for key in rest.lines() {
        if key.len() != 64
            || !key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !entries.insert(key.to_owned())
            || entries.len() > MAX_KEYS
        {
            return Err(invalid());
        }
    }
    Ok(entries)
}

/// Atomic and durable update: private create-new temp inode -> file fsync ->
/// rename -> directory fsync. Never erase the old ledger before publication.
/// An unsuccessful fsync or write reports an error and keeps the in-process
/// revocation fence engaged; no success is claimed without durability.
pub(crate) fn update(path: &Path, token: &str, revoked: bool) -> io::Result<()> {
    update_with_file_sync(path, token, revoked, File::sync_all)
}

// Inject only the real prepublication file fsync for a meaningful rollback
// negative test. The production path always performs both fsync barriers.
fn update_with_file_sync(
    path: &Path,
    token: &str,
    revoked: bool,
    sync_file: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<()> {
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    storage::ensure_private_parent(path).map_err(|_| invalid())?;
    let mut entries = read(path)?;
    let changed = if revoked {
        entries.insert(token.to_owned())
    } else {
        entries.remove(token)
    };
    if !changed {
        return Ok(());
    }
    if entries.len() > MAX_KEYS {
        return Err(invalid());
    }
    let parent = path.parent().ok_or_else(invalid)?;
    let temporary = parent.join(format!(
        ".playlist-lan-revocations-{}.part",
        uuid::Uuid::new_v4()
    ));
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(temporary.clone());
    let mut writer = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temporary)?;
    writer.write_all(HEADER.as_bytes())?;
    let mut ordered: Vec<_> = entries.into_iter().collect();
    ordered.sort_unstable();
    for entry in ordered {
        writer.write_all(entry.as_bytes())?;
        writer.write_all(b"\n")?;
    }
    sync_file(&writer)?;
    drop(writer);
    fs::rename(&temporary, path)?;
    File::open(parent)?.sync_all()
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid LAN permission revocation ledger",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("broadcaster-ledger-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            Self(dir.join(FILENAME))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    #[test]
    fn durable_revoke_survives_simulated_restart_and_requires_new_grant() {
        let f = Fixture::new();
        let token = "a".repeat(64);
        assert!(!is_revoked(&f.0, &token).unwrap());
        update(&f.0, &token, true).unwrap();
        // Reopen the file independently: no process-local deny map is used.
        assert!(is_revoked(&f.0, &token).unwrap());
        assert_eq!(
            fs::metadata(&f.0).unwrap().permissions().mode() & 0o777,
            0o600
        );
        update(&f.0, &token, true).unwrap();
        update(&f.0, &token, false).unwrap();
        assert!(!is_revoked(&f.0, &token).unwrap());
    }

    #[test]
    fn child_process_reopens_revocation_without_in_memory_state() {
        let f = Fixture::new();
        let token = "d".repeat(64);
        update(&f.0, &token, true).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("streams::consent_ledger::tests::cold_start_revocation_probe")
            .env("BROADCASTER_LEDGER_PROBE", &f.0)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "cold-start ledger probe failed: {}",
            String::from_utf8_lossy(&child.stdout)
        );
    }

    #[test]
    fn cold_start_revocation_probe() {
        let Some(path) = std::env::var_os("BROADCASTER_LEDGER_PROBE") else {
            return;
        };
        let token = "d".repeat(64);
        assert!(is_revoked(Path::new(&path), &token).unwrap());
    }

    #[test]
    fn malformed_or_symlink_ledger_denies_instead_of_falling_back() {
        let f = Fixture::new();
        let token = "b".repeat(64);
        fs::write(&f.0, b"not a ledger").unwrap();
        fs::set_permissions(&f.0, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(is_revoked(&f.0, &token).is_err());
        assert!(update(&f.0, &token, false).is_err());
        fs::remove_file(&f.0).unwrap();
        let target = f.0.parent().unwrap().join("target");
        fs::write(&target, HEADER).unwrap();
        symlink(&target, &f.0).unwrap();
        assert!(is_revoked(&f.0, &token).is_err());
    }

    #[test]
    fn interrupted_clear_cannot_replace_a_previous_revocation() {
        let f = Fixture::new();
        let token = "e".repeat(64);
        update(&f.0, &token, true).unwrap();
        let failure = update_with_file_sync(&f.0, &token, false, |_| {
            Err(io::Error::other("synthetic file fsync refusal"))
        });
        assert!(failure.is_err());
        // The completed old ledger is still authoritative after a new read.
        assert!(is_revoked(&f.0, &token).unwrap());
    }

    #[test]
    fn refusal_to_write_record_does_not_report_persisted_revoke() {
        let f = Fixture::new();
        let token = "c".repeat(64);
        fs::create_dir(&f.0).unwrap();
        assert!(update(&f.0, &token, true).is_err());
        assert!(is_revoked(&f.0, &token).is_err());
    }
}
