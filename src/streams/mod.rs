// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Pure playlist parsing and SQLite persistence; integration runs these off GTK's main loop.
//! No streams UI or playback integration is registered here.
#![allow(dead_code)]

pub(crate) mod model;
pub(crate) mod parser;
mod query;
mod refresh;
pub(crate) mod storage;

#[cfg(test)]
mod parser_tests;
#[cfg(test)]
mod tests;
