// SPDX-FileCopyrightText: html2pango contributors
// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Sanitize programme descriptions and keep the simple HTML-to-Pango conversion
//! formerly provided by html2pango 0.6.0.
//!
//! The converter is adapted from html2pango's GPL-3.0-or-later `markup` and
//! `markup_from_raw` functions. The sanitizer is the maintained ammonia 4.x
//! release, with its default allowlist and no CSS/SVG extensions.

use std::{borrow::Cow, sync::LazyLock};

use regex::{Captures, Regex};

static TAGS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat![
        r"<p>|</p>|",
        r"<br>|",
        r"<b>|</b>|",
        r"<strong>|</strong>|",
        r"<code>|</code>|",
        r"<i>|</i>|",
        r"<sub>|</sub>|",
        r"<sup>|</sup>|",
        r"<em>|</em>|",
        r"<|>|",
        r"&nbsp;|",
        r"https?://",
        r#"[^s,)("]+"#,
        r"(?((&amp;)?w+(=[w._-]+)?)*)*",
        r"(#[w._-]+)?",
    ])
    .expect("valid programme markup pattern")
});

/// Convert untrusted HTML into safe, simple Pango markup.
pub fn markup(input: &str) -> String {
    let sanitized = ammonia::Builder::new()
        .link_rel(None)
        .clean(input)
        .to_string();
    markup_from_sanitized(&sanitized)
}

fn markup_from_sanitized(input: &str) -> String {
    TAGS.replace_all(input.trim(), |captures: &Captures<'_>| {
        let matched = captures.get(0).expect("matched capture").as_str();
        match matched {
            "<p>" | "</p>" => Cow::Borrowed(""),
            "<br>" => Cow::Borrowed("\n"),
            "<b>" | "<strong>" => Cow::Borrowed("<b>"),
            "</b>" | "</strong>" => Cow::Borrowed("</b>"),
            "<code>" => Cow::Borrowed("<tt>"),
            "</code>" => Cow::Borrowed("</tt>"),
            "<i>" | "<em>" => Cow::Borrowed("<i>"),
            "</i>" | "</em>" => Cow::Borrowed("</i>"),
            "<sub>" => Cow::Borrowed("<sub>"),
            "</sub>" => Cow::Borrowed("</sub>"),
            "<sup>" => Cow::Borrowed("<sup>"),
            "</sup>" => Cow::Borrowed("</sup>"),
            "<" => Cow::Borrowed("&lt;"),
            ">" => Cow::Borrowed("&gt;"),
            "&nbsp;" => Cow::Borrowed("\u{a0}"),
            _ => Cow::Owned(format!(r#"<a href="{0}">{0}</a>"#, matched)),
        }
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::markup;

    #[test]
    fn retains_existing_description_formatting() {
        assert_eq!(markup("this is parsed"), "this is parsed");
        assert_eq!(
            markup("<p><strong>this <i>is &sanitized<f;><unsupported/></i></strong></p>"),
            "<b>this <i>is &amp;sanitized</i></b>"
        );
        assert_eq!(markup("<code>Text</code>"), "<tt>Text</tt>");
        assert_eq!(markup("a<br>b"), "a\nb");
        assert_eq!(markup("one&nbsp;two"), "one\u{a0}two");
    }

    #[test]
    fn keeps_uri_linkification_and_escapes_unsupported_markup() {
        assert_eq!(
            markup("go to: https://gnome.org"),
            r#"go to: <a href="https://gnome.org">https://gnome.org</a>"#
        );
        assert_eq!(
            markup("this is <span>a tag</span>"),
            "this is &lt;span&gt;a tag&lt;/span&gt;"
        );
    }

    #[test]
    fn rejects_unsafe_css_svg_and_script_attributes() {
        let input = r#"<b>Bold programme</b><span style="color:red">Text</span><svg><a><animateMotion attributeName="href" to="javascript:alert(1)"></animateMotion></a></svg><script>alert(1)</script>"#;
        let rendered = markup(input);
        assert!(rendered.contains("<b>Bold programme</b>"));
        assert!(!rendered.contains("style="));
        assert!(!rendered.contains("javascript:"));
        assert!(!rendered.contains("animateMotion"));
        assert!(!rendered.contains("<script"));
        assert!(!rendered.contains("alert(1)"));
    }

    #[test]
    fn escapes_broken_markup_and_untrusted_angle_brackets() {
        let rendered = markup("<b>safe</b><unknown attr='x'>text</unknown>");
        assert!(rendered.contains("<b>safe</b>"));
        assert!(!rendered.contains("<unknown"));
    }
}
