// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Deterministic, synthetic parser fixtures: never use subscription playlists.
use super::parser::{
    parse_playlist, ParseError, ParsedPlaylist, WarningKind, MAX_ATTRIBUTE_BYTES, MAX_CANDIDATES,
    MAX_LINE_BYTES, MAX_PLAYLIST_BYTES, MAX_URL_BYTES,
};

const STREAM: &str = "https://example.invalid/live/index.m3u8";

fn parsed(input: &str) -> ParsedPlaylist {
    parse_playlist(input.as_bytes(), None).expect("synthetic catalogue parses")
}

fn has_warning(result: &ParsedPlaylist, kind: WarningKind) -> bool {
    result.warnings.iter().any(|warning| warning.kind == kind)
}

#[test]
fn m3u_plain_extended_and_titles_table() {
    for (fixture, titles, groups) in [
        (
            "https://example.invalid/a.m3u8\nhttps://example.invalid/b.m3u8\n",
            vec!["Channel 1", "Channel 2"],
            vec![None, None],
        ),
        (
            "#EXTM3U\n#EXTINF:-1 tvg-name=\"Fallback\",\nhttps://example.invalid/a\n",
            vec!["Fallback"],
            vec![None],
        ),
        (
            "#EXTM3U\n#EXTGRP:Documentaries\n#EXTINF:0 group-title=\"News\",Station\nhttps://example.invalid/a\n#EXTGRP:Sports\nhttps://example.invalid/b\n",
            vec!["Station", "Channel 2"],
            vec![Some("News"), Some("Sports")],
        ),
        (
            "#EXTM3U\n#EXTINF:-1, Title, with commas\nhttps://example.invalid/a\n",
            vec!["Title, with commas"],
            vec![None],
        ),
    ] {
        let actual = parsed(fixture);
        assert_eq!(
            actual.channels.iter().map(|c| c.imported_name.as_str()).collect::<Vec<_>>(),
            titles
        );
        assert_eq!(
            actual.channels.iter().map(|c| c.group_title.as_deref()).collect::<Vec<_>>(),
            groups
        );
        assert_eq!(actual.candidates, titles.len());
        assert_eq!(actual.skipped, 0);
    }
}

#[test]
fn bom_crlf_cr_and_unicode_attributes() {
    let fixture = "\u{feff}#EXTM3U\r\n#EXTINF:-1 tvg-id=\"bürger\" tvg-name=\"Télévision, 日本\" group-title=\"Émissions, 国際\" , Fernsehsender\rhttps://example.invalid/a\r";
    let actual = parsed(fixture);
    let channel = &actual.channels[0];
    assert_eq!(channel.tvg_id.as_deref(), Some("bürger"));
    assert_eq!(channel.tvg_name.as_deref(), Some("Télévision, 日本"));
    assert_eq!(channel.group_title.as_deref(), Some("Émissions, 国際"));
    assert_eq!(channel.imported_name, "Fernsehsender");
}

#[test]
fn quote_tokenizer_spaces_commas_escapes_and_reordered_attributes() {
    let fixture = r#"#EXTM3U
#EXTINF:-1   group-title = "Culture, Arts" tvg-name="A \"quoted\" channel" tvg-id = "ID,7" tvg-chno=14 ,Display
https://example.invalid/a
"#;
    let result = parsed(fixture);
    let channel = &result.channels[0];
    assert_eq!(channel.tvg_id.as_deref(), Some("ID,7"));
    assert_eq!(channel.tvg_name.as_deref(), Some("A \"quoted\" channel"));
    assert_eq!(channel.group_title.as_deref(), Some("Culture, Arts"));
    assert_eq!(channel.channel_number.as_deref(), Some("14"));
    assert_eq!(channel.imported_name, "Display");
}

#[test]
fn metadata_facets_and_region_are_normalized_by_s1() {
    let fixture = r#"#EXTM3U
#EXTINF:-1 tvg-id="station" tvg-language="DE; en; pt-br; sr-latn-rs;bad_#" tvg-country="de;US;gb;UK;ZZ" tvg-region-code="DE-BY" tvg-region="Bayern" ,Name
https://example.invalid/watch
"#;
    let actual = parsed(fixture);
    let entry = &actual.channels[0];
    assert_eq!(
        entry
            .languages
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["de", "en", "pt-BR", "sr-Latn-RS"]
    );
    assert_eq!(
        entry
            .countries
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["DE", "GB", "US"]
    );
    let region = entry.region.as_ref().expect("region retained");
    assert_eq!(region.code.as_deref(), Some("DE-BY"));
    assert_eq!(region.label.as_deref(), Some("Bayern"));
    assert!(has_warning(&actual, WarningKind::InvalidFacet));
}

#[test]
fn duplicates_remain_separate_for_s1_identity_matching() {
    let fixture = "#EXTM3U\n#EXTINF:-1 tvg-id=\"dup\",First\nhttps://example.invalid/a\n#EXTINF:-1 tvg-id=\"dup\",Second\nhttps://example.invalid/b\n";
    let actual = parsed(fixture);
    assert_eq!(actual.channels.len(), 2);
    assert_eq!(actual.channels[0].tvg_id, actual.channels[1].tvg_id);
    assert_eq!(actual.channels[0].order, 1);
    assert_eq!(actual.channels[1].order, 2);
}

#[test]
fn malformed_metadata_skips_bad_entries_and_recovers() {
    let fixture = "#EXTM3U\n#EXTINF:-1,Abandoned\n#EXTINF:-1 tvg-id=\"new\",Valid\nfile:///etc/passwd\n#EXTINF:-1,Recovered\nhttps://example.invalid/ok\n#EXTINF:-1,Missing endpoint";
    let actual = parsed(fixture);
    assert_eq!(actual.candidates, 2);
    assert_eq!(actual.skipped, 3);
    assert_eq!(actual.channels.len(), 1);
    assert_eq!(actual.channels[0].imported_name, "Recovered");
    assert!(has_warning(&actual, WarningKind::IncompleteEntry));
    assert!(has_warning(&actual, WarningKind::InvalidStreamUrl));
}

#[test]
fn no_playable_channels_is_a_hard_error() {
    for fixture in [
        "",
        "#EXTM3U",
        "#EXTM3U\nfile:///etc/passwd",
        "#EXTM3U\n#EXTINF:-1,none",
    ] {
        assert_eq!(
            parse_playlist(fixture.as_bytes(), None).err(),
            Some(ParseError::NoPlayableChannels)
        );
    }
}

#[test]
fn invalid_utf8_and_bom_position_are_safe() {
    assert_eq!(
        parse_playlist(&[0xef, 0xbb, 0xbf, 0xff], None).err(),
        Some(ParseError::InvalidUtf8)
    );
    // A stray BOM is not interpreted as a media URL, but later entries survive.
    let actual = parsed("\u{feff}#EXTM3U\n\u{feff}garbage\nhttps://example.invalid/okay");
    assert_eq!(actual.channels.len(), 1);
    assert_eq!(actual.skipped, 1);
}

#[test]
fn hls_master_media_variant_and_segment_playlists_are_not_catalogues() {
    let cases = [
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXTINF:6,\nsegment.ts\n",
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nvideo/index.m3u8\n",
        "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\"\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nhttps://example.invalid/segment.ts\n",
        "https://example.invalid/ok\n#EXT-X-ENDLIST",
    ];
    for fixture in cases {
        assert_eq!(
            parse_playlist(
                fixture.as_bytes(),
                Some("https://example.invalid/hls/index.m3u8")
            )
            .err(),
            Some(ParseError::HlsManifest)
        );
    }
    // HLS media URLs embedded in actual IPTV entries are ordinary stream URLs.
    assert_eq!(
        parsed(&format!("#EXTM3U\n#EXTINF:-1,TV\n{STREAM}\n")).channels[0].stream_url,
        STREAM
    );
}

#[test]
fn remote_relative_paths_and_logos_use_final_http_origin() {
    let fixture = "#EXTM3U\n#EXTINF:-1 tvg-logo=\"../icons/logo.png?key=logo-token\",Channel\n../stream/index.m3u8?auth=stream-token\n";
    let actual = parse_playlist(
        fixture.as_bytes(),
        Some("https://example.invalid/group/lists/list.m3u?auth=source-token"),
    )
    .expect("relative paths");
    assert_eq!(
        actual.channels[0].stream_url,
        "https://example.invalid/group/stream/index.m3u8?auth=stream-token"
    );
    assert_eq!(
        actual.channels[0].logo_url.as_deref(),
        Some("https://example.invalid/group/icons/logo.png?key=logo-token")
    );
    assert!(actual.warnings.is_empty());
}

#[test]
fn local_relative_rejected_and_unsafe_schemes_are_skipped() {
    let invalid = [
        "../channel.ts",
        "/channel.ts",
        "file:///etc/passwd",
        "data:text/plain,hi",
        "javascript:alert(1)",
        "edl://unsafe",
        "concat://a|b",
        "rtsp://example.invalid/stream",
        "https://example.invalid/path\\evil",
        "http://example.invalid/a b",
        "http:///example.invalid/path",
        "https://example.invalid/$(danger)",
        "https://example.invalid/`danger`",
    ];
    for raw in invalid {
        let fixture = format!("{raw}\n{STREAM}\n");
        let actual = parsed(&fixture);
        assert_eq!(actual.channels.len(), 1, "{raw}");
        assert_eq!(actual.skipped, 1, "{raw}");
    }
    let remote = parse_playlist(
        b"data:text/plain,hi\nhttps://example.invalid/okay\n",
        Some("https://example.invalid/list"),
    )
    .unwrap();
    assert_eq!(remote.channels.len(), 1);
}

#[test]
fn absolute_url_query_tokens_preserved_verbatim_and_never_debugged() {
    let raw = "https://EXAMPLE.invalid:443/watch.m3u8?Auth=canary-secret%2Fvalue&x=1";
    let actual = parsed(raw);
    assert_eq!(actual.channels[0].stream_url, raw);
    let formatted = format!("{:?}", actual.channels[0]);
    assert!(!formatted.contains("canary-secret"));
    assert!(!formatted.contains(raw));
    assert!(!format!("{:?}", actual.warnings).contains("canary-secret"));
    assert!(!format!("{}", ParseError::NoPlayableChannels).contains("canary-secret"));
}

#[test]
fn invalid_base_is_rejected_without_disclosing_locator() {
    for origin in [
        "file:///tmp/list.m3u",
        "data:text/plain,a",
        "http:///no",
        "https://example.invalid/unsafe\\host",
        "https://example.invalid/a b",
        "https://example.invalid/${EXPAND}",
    ] {
        assert_eq!(
            parse_playlist(STREAM.as_bytes(), Some(origin)).err(),
            Some(ParseError::InvalidOrigin)
        );
    }
    assert!(parse_playlist(STREAM.as_bytes(), Some("http://localhost/list.m3u")).is_ok());
}

#[test]
fn optional_bad_logo_and_referrer_are_dropped_without_losing_channel() {
    let fixture = "#EXTM3U\n#EXTINF:-1 tvg-logo=\"file:///private/logo\" referer=\"javascript:bad\" ,News\nhttps://example.invalid/a\n";
    let actual = parsed(fixture);
    let ch = &actual.channels[0];
    assert!(ch.logo_url.is_none());
    assert!(ch.referer.is_none());
    assert!(has_warning(&actual, WarningKind::InvalidLogoUrl));
    assert!(has_warning(&actual, WarningKind::InvalidReferer));
}

#[test]
fn selected_headers_are_data_not_arbitrary_player_options() {
    let fixture = "#EXTM3U\n#EXTINF:-1 user-agent=\"Browser Example\" ,Channel\n#EXTVLCOPT:http-referrer=https://example.invalid/ref\n#EXTVLCOPT:network-caching=999999999\n#KODIPROP:inputstream.adaptive.license_key=secret\nhttps://example.invalid/a\n";
    let actual = parsed(fixture);
    assert_eq!(
        actual.channels[0].user_agent.as_deref(),
        Some("Browser Example")
    );
    assert_eq!(
        actual.channels[0].referer.as_deref(),
        Some("https://example.invalid/ref")
    );
    assert_eq!(actual.channels.len(), 1);
}

#[test]
fn source_label_is_bounded_and_optional() {
    assert_eq!(
        parsed(&format!("#PLAYLIST:Example TV\n{STREAM}\n"))
            .source_label
            .as_deref(),
        Some("Example TV")
    );
    assert_eq!(
        parsed(&format!("#EXTM3U name=\"Example TV\"\n{STREAM}\n"))
            .source_label
            .as_deref(),
        Some("Example TV")
    );
    let input = format!("#PLAYLIST:{}\n{STREAM}\n", "x".repeat(129));
    let actual = parsed(&input);
    assert!(actual.source_label.is_none());
    assert!(has_warning(&actual, WarningKind::InvalidMetadata));
}

#[test]
fn exact_line_limit_passes_overlong_line_is_skipped() {
    let valid = format!("#{}\n{STREAM}\n", "a".repeat(MAX_LINE_BYTES - 1));
    assert_eq!(parsed(&valid).channels.len(), 1);
    let invalid = format!("#{}\n{STREAM}\n", "a".repeat(MAX_LINE_BYTES));
    let actual = parsed(&invalid);
    assert_eq!(actual.channels.len(), 1);
    assert!(has_warning(&actual, WarningKind::OversizedLine));
}

#[test]
fn attribute_limit_exact_and_over_limit() {
    let prefix = "https://example.invalid/";
    let within = format!("{prefix}{}", "a".repeat(MAX_ATTRIBUTE_BYTES - prefix.len()));
    assert_eq!(within.len(), MAX_ATTRIBUTE_BYTES);
    let fixture = format!("#EXTINF:-1 referer=\"{within}\",Channel\n{STREAM}\n");
    let actual = parsed(&fixture);
    assert_eq!(actual.channels[0].referer.as_deref(), Some(within.as_str()));
    let too_long = format!("{within}b");
    let fixture = format!("#EXTINF:-1 referer=\"{too_long}\",Channel\n{STREAM}\n");
    let actual = parsed(&fixture);
    assert!(actual.channels[0].referer.is_none());
    assert!(has_warning(&actual, WarningKind::OversizedAttribute));
}

#[test]
fn url_limit_exact_and_over_limit() {
    let prefix = "https://example.invalid/";
    let within = format!("{prefix}{}", "p".repeat(MAX_URL_BYTES - prefix.len()));
    let result = parsed(&within);
    assert_eq!(result.channels[0].stream_url, within);
    let invalid = format!("{}z\n{STREAM}", within);
    let result = parsed(&invalid);
    assert_eq!(result.channels.len(), 1);
    assert_eq!(result.skipped, 1);
}

#[test]
fn body_limit_exact_and_over_limit() {
    let mut fixture = vec![b'\n'; MAX_PLAYLIST_BYTES];
    let stream = STREAM.as_bytes();
    fixture[..stream.len()].copy_from_slice(stream);
    assert_eq!(parse_playlist(&fixture, None).unwrap().channels.len(), 1);
    fixture.push(b'\n');
    assert_eq!(
        parse_playlist(&fixture, None).err(),
        Some(ParseError::BodyTooLarge)
    );
}

#[test]
fn candidate_limit_exact_and_over_limit() {
    let row = "https://example.invalid/stream.m3u8\n";
    let fixture = row.repeat(MAX_CANDIDATES);
    let actual = parsed(&fixture);
    assert_eq!(actual.channels.len(), MAX_CANDIDATES);
    assert_eq!(actual.candidates, MAX_CANDIDATES);
    let fixture = format!("{fixture}{row}");
    assert_eq!(
        parse_playlist(fixture.as_bytes(), None).err(),
        Some(ParseError::TooManyCandidates)
    );
}

#[test]
fn warnings_are_aggregated_and_never_contain_untrusted_content() {
    let fixture = format!(
        "{}\n{STREAM}\n",
        "file:///canary-private-token\n".repeat(500)
    );
    let actual = parsed(&fixture);
    assert_eq!(actual.skipped, 500);
    assert_eq!(actual.warnings.len(), 1);
    assert_eq!(actual.warnings[0].count, 500);
    assert!(!format!("{:?}", actual.warnings).contains("canary-private-token"));
}

#[test]
fn unknown_attributes_and_alternate_comments_are_ignored() {
    let fixture = "#EXTM3U\n#COMMENT:secret\n#EXTINF:-1 made-up=\"unsafe-player-setting\" tvg-name=\"Valid\",Title\n#OTHEROPT:private\nhttps://example.invalid/a\n";
    let actual = parsed(fixture);
    assert_eq!(actual.channels.len(), 1);
    assert_eq!(actual.channels[0].imported_name, "Title");
    assert_eq!(actual.channels[0].tvg_name.as_deref(), Some("Valid"));
}
