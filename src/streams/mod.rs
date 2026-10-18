// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded IPTV retrieval, parsing, async orchestration, and SQLite persistence.
//! No streams tab, logos, or playback integration is registered here.
#![allow(dead_code)]

pub(crate) mod model;
pub(crate) mod parser;
mod fetch;
pub(crate) mod service;
mod query;
mod refresh;
pub(crate) mod storage;

#[cfg(test)]
mod parser_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod service_tests;
