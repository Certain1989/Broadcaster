// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Production streaming XMLTV limits shared by source transports and importer.
//! These are independent ceilings, not a promise that a device can process the
//! maximal combination. Snapshot decoding deliberately uses smaller caps.
use std::time::Duration;

/// Input transport counts wire bytes, while the decoder also counts expanded
/// XML bytes. Plain XML is limited by the expanded cap, not compressed bytes.
#[derive(Clone, Copy, Debug)]
pub struct StreamingLimits {
    pub compressed_bytes: usize,
    pub expanded_bytes: usize,
    pub ratio: usize,
    /// Raw XMLTV channel *declarations*, before SQLite deduplication.
    pub channels: usize,
    /// Parsed programme records, before SQLite deduplication or omission.
    pub programmes: usize,
}

impl StreamingLimits {
    pub const fn candidate() -> Self {
        Self {
            compressed_bytes: 256 * 1024 * 1024,
            expanded_bytes: 2 * 1024 * 1024 * 1024,
            ratio: 200,
            channels: 100_000,
            programmes: 5_000_000,
        }
    }
}

pub const PRODUCTION: StreamingLimits = StreamingLimits::candidate();
pub const IMPORT_DEADLINE: Duration = Duration::from_secs(90 * 60);
pub const IMPORT_DEADLINE_SECONDS: u32 = 90 * 60;
/// Main SQLite database + WAL, not the TEMP or staging file budget.
pub const DATABASE_AND_WAL_BYTES: u64 = 6 * 1024 * 1024 * 1024;
pub const SQLITE_TEMP_PAGE_BYTES: i64 = 128 * 1024 * 1024;

pub const fn spool_cap(gzip: bool) -> u64 {
    if gzip {
        PRODUCTION.compressed_bytes as u64
    } else {
        PRODUCTION.expanded_bytes as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_support_floor_and_shared_transport_caps() {
        let caps = StreamingLimits::candidate();
        assert_eq!(caps.channels, 100_000);
        assert_eq!(caps.programmes, 5_000_000);
        assert!(caps.channels >= 15_000);
        assert!(caps.programmes >= 1_500_000);
        assert_eq!(spool_cap(true), caps.compressed_bytes as u64);
        assert_eq!(spool_cap(false), caps.expanded_bytes as u64);
        assert_eq!(caps.ratio, 200);
        assert_eq!(
            IMPORT_DEADLINE,
            Duration::from_secs(IMPORT_DEADLINE_SECONDS as u64)
        );
    }
}
