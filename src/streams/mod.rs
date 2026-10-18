// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded IPTV retrieval, parsing, async orchestration, and SQLite persistence.
//! A lazy GTK catalogue is registered alongside the off-main backend.
#![allow(dead_code)]

mod editor;
mod fetch;
mod management;
pub(crate) mod model;
pub(crate) mod parser;
mod query;
mod refresh;
mod rows;
pub(crate) mod service;
pub(crate) mod storage;
pub(crate) mod view;

#[cfg(test)]
mod parser_tests;
#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod tests;
