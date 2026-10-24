// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Platform-neutral XMLTV programme domain and cached SQLite adapter.
//! No GTK, networking, file picking or operating-system event loop belongs here.
pub mod cache;
pub mod matching;
#[cfg(test)]
mod matching_tests;
#[cfg(test)]
mod capacity_bench;
pub mod desktop;
mod headroom;
pub mod ingest;
pub mod model;
pub mod network;
#[cfg(test)]
mod network_tests;
pub mod service;
pub mod sources;
#[cfg(test)]
mod tests;
pub mod xmltv;
