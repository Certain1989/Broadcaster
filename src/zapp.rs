// Copyright (c) 2023-2024 d-k-bo
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

// SPDX-FileCopyrightText: d-k-bo <d-k-bo@mailbox.org>
// SPDX-License-Identifier: MIT

use std::time::Duration;

use adw::glib;
use indexmap::IndexMap;
use serde::{de::DeserializeOwned, Deserialize};
use time::OffsetDateTime;

use crate::config::{APP_ID, PROJECT_URL, VERSION};

const ZAPP_BACKEND_URL: &str = "https://api.zapp.mediathekview.de";
/// Defend against malformed or unexpectedly large upstream JSON responses.
const MAX_CHANNEL_LIST_BYTES: usize = 2 * 1024 * 1024;
const MAX_SHOWS_BYTES: usize = 8 * 1024 * 1024;

/// Bound streaming response consumption rather than relying on Content-Length.
fn append_bounded(buffer: &mut Vec<u8>, bytes: &[u8], max_bytes: usize) -> eyre::Result<()> {
    if bytes.len() > max_bytes.saturating_sub(buffer.len()) {
        eyre::bail!("Zapp response exceeds the allowed size");
    }
    buffer.extend_from_slice(bytes);
    Ok(())
}

async fn decode_bounded<T: DeserializeOwned>(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> eyre::Result<T> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        append_bounded(&mut body, &chunk, max_bytes)?;
    }
    Ok(serde_json::from_slice(&body)?)
}

fn shows_url(channel_id: &ChannelId) -> eyre::Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(&format!("{ZAPP_BACKEND_URL}/v1/shows/"))?;
    // Never let a provider-supplied ID replace the endpoint path or inject
    // query parameters. The URL library encodes it as one path segment.
    url.path_segments_mut()
        .map_err(|_| eyre::eyre!("Invalid Zapp backend URL"))?
        .pop_if_empty()
        .push(channel_id.as_ref());
    Ok(url)
}

#[derive(Debug)]
pub struct Zapp {
    http: reqwest::Client,
}
impl Zapp {
    pub fn new() -> eyre::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(format!("{APP_ID}/{VERSION} ({PROJECT_URL})"))
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(15))
                .build()?,
        })
    }
}
impl Zapp {
    pub async fn channel_info_list(&self) -> eyre::Result<ChannelInfoList> {
        let response = self
            .http
            .get(format!("{ZAPP_BACKEND_URL}/v1/channelInfoList"))
            .send()
            .await?
            .error_for_status()?;
        decode_bounded(response, MAX_CHANNEL_LIST_BYTES).await
    }
    pub async fn shows(&self, channel_id: &ChannelId) -> eyre::Result<ShowsResult> {
        let response = self
            .http
            .get(shows_url(channel_id)?)
            .send()
            .await?
            .error_for_status()?;
        decode_bounded(response, MAX_SHOWS_BYTES).await
    }
}

pub type ChannelInfoList = IndexMap<ChannelId, ChannelInfo>;

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq, Deserialize, glib::ValueDelegate)]
pub struct ChannelId(String);
impl AsRef<str> for ChannelId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_ref())
    }
}
impl From<String> for ChannelId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInfo {
    pub name: String,
    pub stream_url: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ShowsResult {
    Shows(Vec<Show>),
    Error(String),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Show {
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub channel: ChannelId,
    #[serde(with = "time::serde::rfc3339")]
    pub start_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub end_time: OffsetDateTime,
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn standard_channel_id_builds_exact_shows_endpoint() {
        let id = ChannelId::from("das_erste".to_owned());
        let url = shows_url(&id).expect("static backend URL is valid");
        assert_eq!(
            url.as_str(),
            "https://api.zapp.mediathekview.de/v1/shows/das_erste"
        );
    }

    #[test]
    fn channel_id_cannot_rewrite_url_or_add_query_parameters() {
        let id = ChannelId::from("../admin?token=fake&x=1".to_owned());
        let url = shows_url(&id).expect("static backend URL is valid");
        assert_eq!(url.host_str(), Some("api.zapp.mediathekview.de"));
        assert_eq!(url.query(), None);
        assert!(url.path().starts_with("/v1/shows/"));
        assert!(!url.path().starts_with("/v1/admin"));
        assert!(url.as_str().contains("%3Ftoken=fake"));
    }

    #[test]
    fn upstream_size_limit_applies_even_without_content_length() {
        let mut body = Vec::new();
        append_bounded(&mut body, b"123", 4).unwrap();
        assert!(append_bounded(&mut body, b"45", 4).is_err());
        assert_eq!(body, b"123");
        assert!(append_bounded(&mut body, b"4", 4).is_ok());
    }
}
