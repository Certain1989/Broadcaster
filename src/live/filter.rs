// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

//! Filters the channels by name while typing.

use adw::{glib, gtk, prelude::*};

use super::ChannelObject;

/// Returns a filter for the channels whose name matches the text of
/// `entry`. It is updated while typing, without confirming the search.
pub fn channel_filter(entry: &gtk::SearchEntry) -> gtk::CustomFilter {
    let filter = gtk::CustomFilter::new(glib::clone!(
        #[weak]
        entry,
        #[upgrade_or]
        true,
        move |channel| {
            let channel = channel
                .downcast_ref::<ChannelObject>()
                .expect("invalid item type");
            matches(&channel.name(), &entry.text())
        }
    ));

    entry.connect_search_changed({
        let filter = filter.clone();
        move |_| filter.changed(gtk::FilterChange::Different)
    });
    // Escape clears the filter
    entry.connect_stop_search(|entry| entry.set_text(""));

    filter
}

/// Returns `true` if every word of `query` is part of `name`.
///
/// Case and characters other than letters and digits are ignored, so that
/// e.g. “zdf neo” and “zdfneo” find “zdf_neo HD”.
fn matches(name: &str, query: &str) -> bool {
    let name = normalize(name);
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .all(|word| name.contains(&normalize(word)))
}

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn matches_everything_without_query() {
        assert!(matches("Das Erste HD", ""));
        assert!(matches("Das Erste HD", " - "));
    }

    #[test]
    fn ignores_case_and_separators() {
        assert!(matches("zdf_neo HD", "ZDF neo"));
        assert!(matches("zdf_neo HD", "zdfneo"));
        assert!(matches("zdf_neo HD", "zdf-neo"));
        assert!(matches("ARD-alpha", "ard alpha"));
        assert!(matches("BR Fernsehen Süd HD", "SÜD"));
    }

    #[test]
    fn requires_every_word() {
        assert!(matches("Das Erste HD", "erste hd"));
        assert!(matches("Das Erste HD", "hd erste"));
        assert!(!matches("Das Erste HD", "erste sd"));
        assert!(!matches("ZDF HD", "zdfneo"));
    }
}
