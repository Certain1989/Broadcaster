// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared, offline language/country choice UI. Human labels are never stored.
//! Country set mirrors model.rs ISO 3166-1; language list is a discoverable
//! ISO 639 subset. Valid existing BCP47-style tags outside the list survive.
use std::{cell::RefCell, collections::BTreeSet, rc::Rc};

use adw::{gtk, prelude::*};
use gettextrs::gettext;

use super::model::{normalize_facet, FacetDimension};

// CLDR-derived display names for en and de, embedded for offline Flatpak use.
// The country codes come from model::ISO_3166_ALPHA2; do not restrict storage
// to the most common examples in the UI.
const COUNTRY_NAMES: &[(&str, &str, &str)] = &[
    ("AD", "Andorra", "Andorra"),
    ("AE", "United Arab Emirates", "Vereinigte Arabische Emirate"),
    ("AF", "Afghanistan", "Afghanistan"),
    ("AG", "Antigua & Barbuda", "Antigua und Barbuda"),
    ("AI", "Anguilla", "Anguilla"),
    ("AL", "Albania", "Albanien"),
    ("AM", "Armenia", "Armenien"),
    ("AO", "Angola", "Angola"),
    ("AQ", "Antarctica", "Antarktis"),
    ("AR", "Argentina", "Argentinien"),
    ("AS", "American Samoa", "Amerikanisch-Samoa"),
    ("AT", "Austria", "Österreich"),
    ("AU", "Australia", "Australien"),
    ("AW", "Aruba", "Aruba"),
    ("AX", "Åland Islands", "Ålandinseln"),
    ("AZ", "Azerbaijan", "Aserbaidschan"),
    ("BA", "Bosnia & Herzegovina", "Bosnien und Herzegowina"),
    ("BB", "Barbados", "Barbados"),
    ("BD", "Bangladesh", "Bangladesch"),
    ("BE", "Belgium", "Belgien"),
    ("BF", "Burkina Faso", "Burkina Faso"),
    ("BG", "Bulgaria", "Bulgarien"),
    ("BH", "Bahrain", "Bahrain"),
    ("BI", "Burundi", "Burundi"),
    ("BJ", "Benin", "Benin"),
    ("BL", "St. Barthélemy", "St. Barthélemy"),
    ("BM", "Bermuda", "Bermuda"),
    ("BN", "Brunei", "Brunei Darussalam"),
    ("BO", "Bolivia", "Bolivien"),
    ("BQ", "Caribbean Netherlands", "Karibische Niederlande"),
    ("BR", "Brazil", "Brasilien"),
    ("BS", "Bahamas", "Bahamas"),
    ("BT", "Bhutan", "Bhutan"),
    ("BV", "Bouvet Island", "Bouvetinsel"),
    ("BW", "Botswana", "Botsuana"),
    ("BY", "Belarus", "Belarus"),
    ("BZ", "Belize", "Belize"),
    ("CA", "Canada", "Kanada"),
    ("CC", "Cocos (Keeling) Islands", "Kokosinseln"),
    ("CD", "Congo - Kinshasa", "Kongo-Kinshasa"),
    (
        "CF",
        "Central African Republic",
        "Zentralafrikanische Republik",
    ),
    ("CG", "Congo - Brazzaville", "Kongo-Brazzaville"),
    ("CH", "Switzerland", "Schweiz"),
    ("CI", "Côte d’Ivoire", "Côte d’Ivoire"),
    ("CK", "Cook Islands", "Cookinseln"),
    ("CL", "Chile", "Chile"),
    ("CM", "Cameroon", "Kamerun"),
    ("CN", "China", "China"),
    ("CO", "Colombia", "Kolumbien"),
    ("CR", "Costa Rica", "Costa Rica"),
    ("CU", "Cuba", "Kuba"),
    ("CV", "Cape Verde", "Cabo Verde"),
    ("CW", "Curaçao", "Curaçao"),
    ("CX", "Christmas Island", "Weihnachtsinsel"),
    ("CY", "Cyprus", "Zypern"),
    ("CZ", "Czechia", "Tschechien"),
    ("DE", "Germany", "Deutschland"),
    ("DJ", "Djibouti", "Dschibuti"),
    ("DK", "Denmark", "Dänemark"),
    ("DM", "Dominica", "Dominica"),
    ("DO", "Dominican Republic", "Dominikanische Republik"),
    ("DZ", "Algeria", "Algerien"),
    ("EC", "Ecuador", "Ecuador"),
    ("EE", "Estonia", "Estland"),
    ("EG", "Egypt", "Ägypten"),
    ("EH", "Western Sahara", "Westsahara"),
    ("ER", "Eritrea", "Eritrea"),
    ("ES", "Spain", "Spanien"),
    ("ET", "Ethiopia", "Äthiopien"),
    ("FI", "Finland", "Finnland"),
    ("FJ", "Fiji", "Fidschi"),
    (
        "FK",
        "Falkland Islands (Islas Malvinas)",
        "Falklandinseln (Malwinen)",
    ),
    ("FM", "Micronesia", "Mikronesien"),
    ("FO", "Faroe Islands", "Färöer"),
    ("FR", "France", "Frankreich"),
    ("GA", "Gabon", "Gabun"),
    ("GB", "United Kingdom", "Vereinigtes Königreich"),
    ("GD", "Grenada", "Grenada"),
    ("GE", "Georgia", "Georgien"),
    ("GF", "French Guiana", "Französisch-Guayana"),
    ("GG", "Guernsey", "Guernsey"),
    ("GH", "Ghana", "Ghana"),
    ("GI", "Gibraltar", "Gibraltar"),
    ("GL", "Greenland", "Grönland"),
    ("GM", "Gambia", "Gambia"),
    ("GN", "Guinea", "Guinea"),
    ("GP", "Guadeloupe", "Guadeloupe"),
    ("GQ", "Equatorial Guinea", "Äquatorialguinea"),
    ("GR", "Greece", "Griechenland"),
    (
        "GS",
        "South Georgia & South Sandwich Islands",
        "Südgeorgien und die Südlichen Sandwichinseln",
    ),
    ("GT", "Guatemala", "Guatemala"),
    ("GU", "Guam", "Guam"),
    ("GW", "Guinea-Bissau", "Guinea-Bissau"),
    ("GY", "Guyana", "Guyana"),
    ("HK", "Hong Kong", "Hongkong"),
    ("HM", "Heard & McDonald Islands", "Heard und McDonaldinseln"),
    ("HN", "Honduras", "Honduras"),
    ("HR", "Croatia", "Kroatien"),
    ("HT", "Haiti", "Haiti"),
    ("HU", "Hungary", "Ungarn"),
    ("ID", "Indonesia", "Indonesien"),
    ("IE", "Ireland", "Irland"),
    ("IL", "Israel", "Israel"),
    ("IM", "Isle of Man", "Isle of Man"),
    ("IN", "India", "Indien"),
    (
        "IO",
        "British Indian Ocean Territory",
        "Britisches Territorium im Indischen Ozean",
    ),
    ("IQ", "Iraq", "Irak"),
    ("IR", "Iran", "Iran"),
    ("IS", "Iceland", "Island"),
    ("IT", "Italy", "Italien"),
    ("JE", "Jersey", "Jersey"),
    ("JM", "Jamaica", "Jamaika"),
    ("JO", "Jordan", "Jordanien"),
    ("JP", "Japan", "Japan"),
    ("KE", "Kenya", "Kenia"),
    ("KG", "Kyrgyzstan", "Kirgisistan"),
    ("KH", "Cambodia", "Kambodscha"),
    ("KI", "Kiribati", "Kiribati"),
    ("KM", "Comoros", "Komoren"),
    ("KN", "St. Kitts & Nevis", "St. Kitts und Nevis"),
    ("KP", "North Korea", "Nordkorea"),
    ("KR", "South Korea", "Südkorea"),
    ("KW", "Kuwait", "Kuwait"),
    ("KY", "Cayman Islands", "Kaimaninseln"),
    ("KZ", "Kazakhstan", "Kasachstan"),
    ("LA", "Laos", "Laos"),
    ("LB", "Lebanon", "Libanon"),
    ("LC", "St. Lucia", "St. Lucia"),
    ("LI", "Liechtenstein", "Liechtenstein"),
    ("LK", "Sri Lanka", "Sri Lanka"),
    ("LR", "Liberia", "Liberia"),
    ("LS", "Lesotho", "Lesotho"),
    ("LT", "Lithuania", "Litauen"),
    ("LU", "Luxembourg", "Luxemburg"),
    ("LV", "Latvia", "Lettland"),
    ("LY", "Libya", "Libyen"),
    ("MA", "Morocco", "Marokko"),
    ("MC", "Monaco", "Monaco"),
    ("MD", "Moldova", "Republik Moldau"),
    ("ME", "Montenegro", "Montenegro"),
    ("MF", "St. Martin", "St. Martin"),
    ("MG", "Madagascar", "Madagaskar"),
    ("MH", "Marshall Islands", "Marshallinseln"),
    ("MK", "North Macedonia", "Nordmazedonien"),
    ("ML", "Mali", "Mali"),
    ("MM", "Myanmar (Burma)", "Myanmar"),
    ("MN", "Mongolia", "Mongolei"),
    ("MO", "Macao", "Macau"),
    ("MP", "Northern Mariana Islands", "Nördliche Marianen"),
    ("MQ", "Martinique", "Martinique"),
    ("MR", "Mauritania", "Mauretanien"),
    ("MS", "Montserrat", "Montserrat"),
    ("MT", "Malta", "Malta"),
    ("MU", "Mauritius", "Mauritius"),
    ("MV", "Maldives", "Malediven"),
    ("MW", "Malawi", "Malawi"),
    ("MX", "Mexico", "Mexiko"),
    ("MY", "Malaysia", "Malaysia"),
    ("MZ", "Mozambique", "Mosambik"),
    ("NA", "Namibia", "Namibia"),
    ("NC", "New Caledonia", "Neukaledonien"),
    ("NE", "Niger", "Niger"),
    ("NF", "Norfolk Island", "Norfolkinsel"),
    ("NG", "Nigeria", "Nigeria"),
    ("NI", "Nicaragua", "Nicaragua"),
    ("NL", "Netherlands", "Niederlande"),
    ("NO", "Norway", "Norwegen"),
    ("NP", "Nepal", "Nepal"),
    ("NR", "Nauru", "Nauru"),
    ("NU", "Niue", "Niue"),
    ("NZ", "New Zealand", "Neuseeland"),
    ("OM", "Oman", "Oman"),
    ("PA", "Panama", "Panama"),
    ("PE", "Peru", "Peru"),
    ("PF", "French Polynesia", "Französisch-Polynesien"),
    ("PG", "Papua New Guinea", "Papua-Neuguinea"),
    ("PH", "Philippines", "Philippinen"),
    ("PK", "Pakistan", "Pakistan"),
    ("PL", "Poland", "Polen"),
    ("PM", "St. Pierre & Miquelon", "St. Pierre und Miquelon"),
    ("PN", "Pitcairn Islands", "Pitcairninseln"),
    ("PR", "Puerto Rico", "Puerto Rico"),
    ("PS", "Palestine", "Palästina"),
    ("PT", "Portugal", "Portugal"),
    ("PW", "Palau", "Palau"),
    ("PY", "Paraguay", "Paraguay"),
    ("QA", "Qatar", "Katar"),
    ("RE", "Réunion", "Réunion"),
    ("RO", "Romania", "Rumänien"),
    ("RS", "Serbia", "Serbien"),
    ("RU", "Russia", "Russland"),
    ("RW", "Rwanda", "Ruanda"),
    ("SA", "Saudi Arabia", "Saudi-Arabien"),
    ("SB", "Solomon Islands", "Salomonen"),
    ("SC", "Seychelles", "Seychellen"),
    ("SD", "Sudan", "Sudan"),
    ("SE", "Sweden", "Schweden"),
    ("SG", "Singapore", "Singapur"),
    ("SH", "St. Helena", "St. Helena"),
    ("SI", "Slovenia", "Slowenien"),
    ("SJ", "Svalbard & Jan Mayen", "Spitzbergen und Jan Mayen"),
    ("SK", "Slovakia", "Slowakei"),
    ("SL", "Sierra Leone", "Sierra Leone"),
    ("SM", "San Marino", "San Marino"),
    ("SN", "Senegal", "Senegal"),
    ("SO", "Somalia", "Somalia"),
    ("SR", "Suriname", "Suriname"),
    ("SS", "South Sudan", "Südsudan"),
    ("ST", "São Tomé & Príncipe", "São Tomé und Príncipe"),
    ("SV", "El Salvador", "El Salvador"),
    ("SX", "Sint Maarten", "Sint Maarten"),
    ("SY", "Syria", "Syrien"),
    ("SZ", "Eswatini", "Eswatini"),
    ("TC", "Turks & Caicos Islands", "Turks- und Caicosinseln"),
    ("TD", "Chad", "Tschad"),
    (
        "TF",
        "French Southern Territories",
        "Französische Süd- und Antarktisgebiete",
    ),
    ("TG", "Togo", "Togo"),
    ("TH", "Thailand", "Thailand"),
    ("TJ", "Tajikistan", "Tadschikistan"),
    ("TK", "Tokelau", "Tokelau"),
    ("TL", "Timor-Leste", "Timor-Leste"),
    ("TM", "Turkmenistan", "Turkmenistan"),
    ("TN", "Tunisia", "Tunesien"),
    ("TO", "Tonga", "Tonga"),
    ("TR", "Türkiye", "Türkei"),
    ("TT", "Trinidad & Tobago", "Trinidad und Tobago"),
    ("TV", "Tuvalu", "Tuvalu"),
    ("TW", "Taiwan", "Taiwan"),
    ("TZ", "Tanzania", "Tansania"),
    ("UA", "Ukraine", "Ukraine"),
    ("UG", "Uganda", "Uganda"),
    ("UM", "U.S. Outlying Islands", "Amerikanische Überseeinseln"),
    ("US", "United States", "Vereinigte Staaten"),
    ("UY", "Uruguay", "Uruguay"),
    ("UZ", "Uzbekistan", "Usbekistan"),
    ("VA", "Vatican City", "Vatikanstadt"),
    (
        "VC",
        "St. Vincent & Grenadines",
        "St. Vincent und die Grenadinen",
    ),
    ("VE", "Venezuela", "Venezuela"),
    ("VG", "British Virgin Islands", "Britische Jungferninseln"),
    ("VI", "U.S. Virgin Islands", "Amerikanische Jungferninseln"),
    ("VN", "Vietnam", "Vietnam"),
    ("VU", "Vanuatu", "Vanuatu"),
    ("WF", "Wallis & Futuna", "Wallis und Futuna"),
    ("WS", "Samoa", "Samoa"),
    ("YE", "Yemen", "Jemen"),
    ("YT", "Mayotte", "Mayotte"),
    ("ZA", "South Africa", "Südafrika"),
    ("ZM", "Zambia", "Sambia"),
    ("ZW", "Zimbabwe", "Simbabwe"),
];
const LANGUAGE_NAMES: &[(&str, &str, &str)] = &[
    ("aa", "aa", "aa"),
    ("ab", "ab", "ab"),
    ("ae", "ae", "ae"),
    ("af", "Afrikaans", "Afrikaans"),
    ("ak", "Akan", "Akan"),
    ("am", "Amharic", "Amharisch"),
    ("an", "Aragonese", "Aragonesisch"),
    ("ar", "Arabic", "Arabisch"),
    ("as", "Assamese", "Assamesisch"),
    ("av", "av", "av"),
    ("ay", "Aymara", "Aymara"),
    ("az", "Azerbaijani", "Aserbaidschanisch"),
    ("ba", "ba", "ba"),
    ("be", "Belarusian", "Belarussisch"),
    ("bg", "Bulgarian", "Bulgarisch"),
    ("bh", "Bhojpuri", "Bhodschpuri"),
    ("bi", "bi", "bi"),
    ("bm", "Bambara", "Bambara"),
    ("bn", "Bangla", "Bengalisch"),
    ("bo", "bo", "bo"),
    ("br", "Breton", "Bretonisch"),
    ("bs", "Bosnian", "Bosnisch"),
    ("ca", "Catalan", "Katalanisch"),
    ("ce", "ce", "ce"),
    ("ch", "ch", "ch"),
    ("co", "Corsican", "Korsisch"),
    ("cr", "cr", "cr"),
    ("cs", "Czech", "Tschechisch"),
    ("cu", "cu", "cu"),
    ("cv", "cv", "cv"),
    ("cy", "Welsh", "Walisisch"),
    ("da", "Danish", "Dänisch"),
    ("de", "German", "Deutsch"),
    ("dv", "Divehi", "Dhivehi"),
    ("dz", "dz", "dz"),
    ("ee", "Ewe", "Ewe"),
    ("el", "Greek", "Griechisch"),
    ("en", "English", "Englisch"),
    ("eo", "Esperanto", "Esperanto"),
    ("es", "Spanish", "Spanisch"),
    ("et", "Estonian", "Estnisch"),
    ("eu", "Basque", "Baskisch"),
    ("fa", "Persian", "Persisch"),
    ("ff", "ff", "ff"),
    ("fi", "Finnish", "Finnisch"),
    ("fj", "fj", "fj"),
    ("fo", "Faroese", "Färöisch"),
    ("fr", "French", "Französisch"),
    ("fy", "Western Frisian", "Westfriesisch"),
    ("ga", "Irish", "Irisch"),
    ("gd", "Scottish Gaelic", "Gälisch (Schottland)"),
    ("gl", "Galician", "Galicisch"),
    ("gn", "Guarani", "Guaraní"),
    ("gu", "Gujarati", "Gujarati"),
    ("gv", "gv", "gv"),
    ("ha", "Hausa", "Haussa"),
    ("he", "Hebrew", "Hebräisch"),
    ("hi", "Hindi", "Hindi"),
    ("ho", "ho", "ho"),
    ("hr", "Croatian", "Kroatisch"),
    ("ht", "Haitian Creole", "Haiti-Kreolisch"),
    ("hu", "Hungarian", "Ungarisch"),
    ("hy", "Armenian", "Armenisch"),
    ("hz", "hz", "hz"),
    ("ia", "Interlingua", "Interlingua"),
    ("id", "Indonesian", "Indonesisch"),
    ("ie", "ie", "ie"),
    ("ig", "Igbo", "Igbo"),
    ("ii", "ii", "ii"),
    ("ik", "ik", "ik"),
    ("io", "io", "io"),
    ("is", "Icelandic", "Isländisch"),
    ("it", "Italian", "Italienisch"),
    ("iu", "iu", "iu"),
    ("ja", "Japanese", "Japanisch"),
    ("jv", "Javanese", "Javanisch"),
    ("ka", "Georgian", "Georgisch"),
    ("kg", "kg", "kg"),
    ("ki", "ki", "ki"),
    ("kj", "kj", "kj"),
    ("kk", "Kazakh", "Kasachisch"),
    ("kl", "kl", "kl"),
    ("km", "Khmer", "Khmer"),
    ("kn", "Kannada", "Kannada"),
    ("ko", "Korean", "Koreanisch"),
    ("kr", "kr", "kr"),
    ("ks", "ks", "ks"),
    ("ku", "Kurdish", "Kurdisch"),
    ("kv", "kv", "kv"),
    ("kw", "kw", "kw"),
    ("ky", "Kyrgyz", "Kirgisisch"),
    ("la", "Latin", "Latein"),
    ("lb", "Luxembourgish", "Luxemburgisch"),
    ("lg", "Ganda", "Ganda"),
    ("li", "li", "li"),
    ("ln", "Lingala", "Lingala"),
    ("lo", "Lao", "Laotisch"),
    ("lt", "Lithuanian", "Litauisch"),
    ("lu", "lu", "lu"),
    ("lv", "Latvian", "Lettisch"),
    ("mg", "Malagasy", "Malagasy"),
    ("mh", "mh", "mh"),
    ("mi", "Māori", "Māori"),
    ("mk", "Macedonian", "Mazedonisch"),
    ("ml", "Malayalam", "Malayalam"),
    ("mn", "Mongolian", "Mongolisch"),
    ("mr", "Marathi", "Marathi"),
    ("ms", "Malay", "Malaiisch"),
    ("mt", "Maltese", "Maltesisch"),
    ("my", "Burmese", "Birmanisch"),
    ("na", "na", "na"),
    ("nb", "Norwegian Bokmål", "Norwegisch (Bokmål)"),
    ("nd", "nd", "nd"),
    ("ne", "Nepali", "Nepalesisch"),
    ("ng", "ng", "ng"),
    ("nl", "Dutch", "Niederländisch"),
    ("nn", "Norwegian Nynorsk", "Norwegisch (Nynorsk)"),
    ("no", "Norwegian", "Norwegisch"),
    ("nr", "nr", "nr"),
    ("nv", "nv", "nv"),
    ("ny", "Nyanja", "Nyanja"),
    ("oc", "Occitan", "Okzitanisch"),
    ("oj", "oj", "oj"),
    ("om", "Oromo", "Oromo"),
    ("or", "Odia", "Oriya"),
    ("os", "os", "os"),
    ("pa", "Punjabi", "Punjabi"),
    ("pi", "pi", "pi"),
    ("pl", "Polish", "Polnisch"),
    ("ps", "Pashto", "Paschtu"),
    ("pt", "Portuguese", "Portugiesisch"),
    ("qu", "Quechua", "Quechua"),
    ("rm", "Romansh", "Rätoromanisch"),
    ("rn", "rn", "rn"),
    ("ro", "Romanian", "Rumänisch"),
    ("ru", "Russian", "Russisch"),
    ("rw", "Kinyarwanda", "Kinyarwanda"),
    ("sa", "Sanskrit", "Sanskrit"),
    ("sc", "sc", "sc"),
    ("sd", "Sindhi", "Sindhi"),
    ("se", "se", "se"),
    ("sg", "sg", "sg"),
    ("si", "Sinhala", "Singhalesisch"),
    ("sk", "Slovak", "Slowakisch"),
    ("sl", "Slovenian", "Slowenisch"),
    ("sm", "Samoan", "Samoanisch"),
    ("sn", "Shona", "Shona"),
    ("so", "Somali", "Somali"),
    ("sq", "Albanian", "Albanisch"),
    ("sr", "Serbian", "Serbisch"),
    ("ss", "ss", "ss"),
    ("st", "Southern Sotho", "Süd-Sotho"),
    ("su", "Sundanese", "Sundanesisch"),
    ("sv", "Swedish", "Schwedisch"),
    ("sw", "Swahili", "Suaheli"),
    ("ta", "Tamil", "Tamil"),
    ("te", "Telugu", "Telugu"),
    ("tg", "Tajik", "Tadschikisch"),
    ("th", "Thai", "Thailändisch"),
    ("ti", "Tigrinya", "Tigrinya"),
    ("tk", "Turkmen", "Turkmenisch"),
    ("tl", "Filipino", "Filipino"),
    ("tn", "Tswana", "Tswana"),
    ("to", "Tongan", "Tongaisch"),
    ("tr", "Turkish", "Türkisch"),
    ("ts", "Tsonga", "Tsonga"),
    ("tt", "Tatar", "Tatarisch"),
    ("tw", "Akan", "Akan"),
    ("ty", "ty", "ty"),
    ("ug", "Uyghur", "Uigurisch"),
    ("uk", "Ukrainian", "Ukrainisch"),
    ("ur", "Urdu", "Urdu"),
    ("uz", "Uzbek", "Usbekisch"),
    ("ve", "ve", "ve"),
    ("vi", "Vietnamese", "Vietnamesisch"),
    ("vo", "vo", "vo"),
    ("wa", "Walloon", "Wallonisch"),
    ("wo", "Wolof", "Wolof"),
    ("xh", "Xhosa", "Xhosa"),
    ("yi", "Yiddish", "Jiddisch"),
    ("yo", "Yoruba", "Yoruba"),
    ("za", "za", "za"),
    ("zh", "Chinese", "Chinesisch"),
    ("zu", "Zulu", "Zulu"),
];

fn names(kind: FacetDimension) -> &'static [(&'static str, &'static str, &'static str)] {
    match kind {
        FacetDimension::Language => LANGUAGE_NAMES,
        FacetDimension::Country => COUNTRY_NAMES,
    }
}

fn german_ui() -> bool {
    let locale = ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|key| std::env::var(key).ok().filter(|s| !s.is_empty()))
        .unwrap_or_default();
    locale.split(':').next().unwrap_or("").starts_with("de")
}

fn human_label(kind: FacetDimension, code: &str) -> String {
    match names(kind).iter().find(|(value, _, _)| *value == code) {
        Some((_, _en, de)) if german_ui() && *de != code => format!("{de} ({code})"),
        Some((_, en, _)) if *en != code => format!("{en} ({code})"),
        _ => code.to_owned(), // Preserve legacy/unrecognized tag, do not guess.
    }
}

fn validate_manual_language(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(gettext("Enter a language tag, for example de-DE."));
    }
    // A word such as "German" happens to pass the storage syntax check as
    // "german". Do not create that misleading pseudo-language in manual UI.
    if LANGUAGE_NAMES.iter().any(|(code, en, de)| {
        !raw.eq_ignore_ascii_case(code)
            && (raw.eq_ignore_ascii_case(en) || raw.eq_ignore_ascii_case(de))
    }) {
        return Err(gettext(
            "Use the named language chooser; manual entry needs a tag such as de-DE.",
        ));
    }
    normalize_facet(raw, FacetDimension::Language).map_err(|_| {
        gettext("Invalid language tag. Use de, en or a tag such as de-DE (not a country).")
    })
}

fn render_selected(
    box_: &gtk::Box,
    kind: FacetDimension,
    selected: &Rc<RefCell<BTreeSet<String>>>,
    checks: &Rc<RefCell<Vec<(String, gtk::CheckButton)>>>,
) {
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }
    let values = selected.borrow().clone();
    if values.is_empty() {
        let empty = gtk::Label::new(Some(&gettext("None selected (optional)")));
        empty.set_xalign(0.0);
        empty.add_css_class("dim-label");
        box_.append(&empty);
    }
    for code in values {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let description = human_label(kind, &code);
        let name = gtk::Label::new(Some(&description));
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_hexpand(true);
        name.set_xalign(0.0);
        item.append(&name);
        let remove = gtk::Button::from_icon_name("list-remove-symbolic");
        remove.set_tooltip_text(Some(&format!("{}: {description}", gettext("Remove"))));
        item.append(&remove);
        box_.append(&item);
        let selected = selected.clone();
        let checks = checks.clone();
        let box_weak = box_.downgrade();
        remove.connect_clicked(move |_| {
            selected.borrow_mut().remove(&code);
            for (known, button) in checks.borrow().iter() {
                if known == &code {
                    button.set_active(false);
                }
            }
            if let Some(box_) = box_weak.upgrade() {
                render_selected(&box_, kind, &selected, &checks);
            }
        });
    }
}

pub(crate) struct FacetPicker {
    pub(crate) root: gtk::Box,
    kind: FacetDimension,
    selected: Rc<RefCell<BTreeSet<String>>>,
    checks: Rc<RefCell<Vec<(String, gtk::CheckButton)>>>,
    selected_box: gtk::Box,
    error: gtk::Label,
    chooser: gtk::Expander,
    manual: Option<(gtk::Entry, gtk::Expander)>,
}

impl FacetPicker {
    pub(crate) fn new(kind: FacetDimension, initial: &BTreeSet<String>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.set_hexpand(true);
        let selected = Rc::new(RefCell::new(initial.clone()));
        let checks = Rc::new(RefCell::new(Vec::new()));
        let selected_box = gtk::Box::new(gtk::Orientation::Vertical, 3);
        root.append(&selected_box);
        let chooser = gtk::Expander::new(Some(&match kind {
            FacetDimension::Language => gettext("Choose languages by name"),
            FacetDimension::Country => gettext("Choose countries by name"),
        }));
        chooser.set_hexpand(true);
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let hint = gtk::Label::new(Some(&gettext(
            "Open the list and use arrow keys and Enter/Space to select or deselect; typing is optional.",
        )));
        hint.set_wrap(true);
        hint.set_xalign(0.0);
        panel.append(&hint);
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some(&gettext("Search names or codes (optional)")));
        panel.append(&search);
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");
        // Include already persisted uncommon tags; they must remain removable.
        let mut candidates: Vec<String> = names(kind)
            .iter()
            .map(|(c, _, _)| (*c).to_owned())
            .collect();
        for code in initial {
            if !candidates.contains(code) {
                candidates.push(code.clone());
            }
        }
        // Remote users can reach common choices immediately without typing.
        let favorites: &[&str] = match kind {
            FacetDimension::Language => &["de", "en", "fr", "es", "it"],
            FacetDimension::Country => &["DE", "AT", "CH", "GB", "US"],
        };
        candidates.sort_by_key(|code| {
            (
                favorites
                    .iter()
                    .position(|favorite| *favorite == code)
                    .unwrap_or(usize::MAX),
                human_label(kind, code).to_lowercase(),
            )
        });
        let error = gtk::Label::new(None);
        error.set_wrap(true);
        error.set_xalign(0.0);
        error.add_css_class("error");
        for code in candidates {
            let check = gtk::CheckButton::with_label(&human_label(kind, &code));
            check.set_active(initial.contains(&code));
            check.set_hexpand(true);
            list.append(&check);
            checks.borrow_mut().push((code.clone(), check.clone()));
            let code_for_keys = code.clone();
            let key_control = gtk::EventControllerKey::new();
            let checks_for_keys = Rc::downgrade(&checks);
            let check_for_keys = check.downgrade();
            let chooser_for_keys = chooser.downgrade();
            key_control.connect_key_pressed(move |_, key, _, modifiers| {
                if !modifiers.is_empty() {
                    return gtk::glib::Propagation::Proceed;
                }
                if matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter) {
                    if let Some(button) = check_for_keys.upgrade() {
                        button.set_active(!button.is_active());
                        return gtk::glib::Propagation::Stop;
                    }
                }
                let direction: i32 = match key {
                    gtk::gdk::Key::Down => 1,
                    gtk::gdk::Key::Up => -1,
                    _ => return gtk::glib::Propagation::Proceed,
                };
                if let Some(checks) = checks_for_keys.upgrade() {
                    let items = checks.borrow();
                    if let Some(index) = items.iter().position(|(name, _)| name == &code_for_keys) {
                        let mut next = index as i32 + direction;
                        while next >= 0 && (next as usize) < items.len() {
                            let target = &items[next as usize].1;
                            if target.parent().is_some_and(|row| row.is_visible())
                                && target.grab_focus()
                            {
                                return gtk::glib::Propagation::Stop;
                            }
                            next += direction;
                        }
                        if direction < 0
                            && chooser_for_keys.upgrade().is_some_and(|w| w.grab_focus())
                        {
                            return gtk::glib::Propagation::Stop;
                        }
                    }
                }
                gtk::glib::Propagation::Proceed
            });
            check.add_controller(key_control);
            let selected = selected.clone();
            let checks_weak = Rc::downgrade(&checks);
            let selected_box_weak = selected_box.downgrade();
            let error_weak = error.downgrade();
            check.connect_toggled(move |button| {
                if button.is_active() {
                    if selected.borrow().len() >= 64 && !selected.borrow().contains(&code) {
                        if let Some(error) = error_weak.upgrade() {
                            error.set_text(&gettext("Choose at most 64 values."));
                        }
                        button.set_active(false);
                        return;
                    }
                    selected.borrow_mut().insert(code.clone());
                } else if !selected.borrow_mut().remove(&code) {
                    return;
                }
                if let Some(error) = error_weak.upgrade() {
                    error.set_text("");
                }
                if let (Some(selected_box), Some(checks)) =
                    (selected_box_weak.upgrade(), checks_weak.upgrade())
                {
                    render_selected(&selected_box, kind, &selected, &checks);
                }
            });
        }
        let chooser_key = gtk::EventControllerKey::new();
        let chooser_weak = chooser.downgrade();
        let checks_weak = Rc::downgrade(&checks);
        chooser_key.connect_key_pressed(move |_, key, _, modifiers| {
            if key == gtk::gdk::Key::Down
                && modifiers.is_empty()
                && chooser_weak
                    .upgrade()
                    .is_some_and(|widget| widget.has_focus())
            {
                if let Some(checks) = checks_weak.upgrade() {
                    if let Some((_, first)) = checks
                        .borrow()
                        .iter()
                        .find(|(_, button)| button.parent().is_some_and(|row| row.is_visible()))
                    {
                        if first.grab_focus() {
                            return gtk::glib::Propagation::Stop;
                        }
                    }
                }
            }
            gtk::glib::Propagation::Proceed
        });
        chooser.add_controller(chooser_key);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_size_request(-1, 196);
        scroller.set_child(Some(&list));
        panel.append(&scroller);
        let search_weak = search.downgrade();
        list.set_filter_func(move |row| {
            let needle = search_weak
                .upgrade()
                .map(|search| search.text().to_lowercase())
                .unwrap_or_default();
            if needle.is_empty() {
                return true;
            }
            row.child()
                .and_downcast::<gtk::CheckButton>()
                .and_then(|check| check.label())
                .is_some_and(|label| label.to_lowercase().contains(&needle))
        });
        let list_weak = list.downgrade();
        search.connect_search_changed(move |_| {
            if let Some(list) = list_weak.upgrade() {
                list.invalidate_filter();
            }
        });
        let manual = if kind == FacetDimension::Language {
            let advanced =
                gtk::Expander::new(Some(&gettext("Advanced: add a language tag manually")));
            let manual_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let entry = gtk::Entry::new();
            entry.set_placeholder_text(Some(&gettext("Example: de-DE (language tag)")));
            entry.set_hexpand(true);
            let add = gtk::Button::with_label(&gettext("Add tag"));
            manual_row.append(&entry);
            manual_row.append(&add);
            let manual_error = gtk::Label::new(None);
            manual_error.set_wrap(true);
            manual_error.set_xalign(0.0);
            manual_error.add_css_class("error");
            let manual_box = gtk::Box::new(gtk::Orientation::Vertical, 3);
            manual_box.append(&manual_row);
            manual_box.append(&manual_error);
            advanced.set_child(Some(&manual_box));
            panel.append(&advanced);
            let selected = selected.clone();
            let checks = checks.clone();
            let selected_box = selected_box.clone();
            let entry_for_click = entry.clone();
            add.connect_clicked(
                move |_| match validate_manual_language(&entry_for_click.text()) {
                    Ok(code)
                        if selected.borrow().len() >= 64 && !selected.borrow().contains(&code) =>
                    {
                        manual_error.set_text(&gettext("Choose at most 64 languages."))
                    }
                    Ok(code) => {
                        selected.borrow_mut().insert(code.clone());
                        for (known, button) in checks.borrow().iter() {
                            if known == &code {
                                button.set_active(true);
                            }
                        }
                        render_selected(&selected_box, kind, &selected, &checks);
                        manual_error.set_text("");
                        entry_for_click.set_text("");
                    }
                    Err(error) => {
                        manual_error.set_text(&error);
                        entry_for_click.grab_focus();
                    }
                },
            );
            Some((entry, advanced))
        } else {
            None
        };
        chooser.set_child(Some(&panel));
        root.append(&chooser);
        root.append(&error);
        render_selected(&selected_box, kind, &selected, &checks);
        Self {
            root,
            kind,
            selected,
            checks,
            selected_box,
            error,
            chooser,
            manual,
        }
    }

    pub(crate) fn values(&self, required: bool) -> Result<BTreeSet<String>, String> {
        if let Some((manual, advanced)) = &self.manual {
            if !manual.text().trim().is_empty() {
                let reason = gettext("Add or clear the typed language tag before saving.");
                self.error.set_text(&reason);
                self.chooser.set_expanded(true);
                advanced.set_expanded(true);
                manual.grab_focus();
                return Err(reason);
            }
        }
        let values = self.selected.borrow().clone();
        if required && values.is_empty() {
            self.error
                .set_text(&gettext("Select at least one value from the named list."));
            self.chooser.set_expanded(true);
            self.chooser.grab_focus();
            return Err(gettext("Select at least one value."));
        }
        self.error.set_text("");
        Ok(values)
    }

    #[cfg(test)]
    pub(crate) fn choose(&self, code: &str) {
        let check = self
            .checks
            .borrow()
            .iter()
            .find(|(known, _)| known == code)
            .map(|(_, check)| check.clone());
        if let Some(check) = check {
            // Exercise the same toggled signal as keyboard/remote selection.
            check.set_active(true);
        } else {
            self.selected.borrow_mut().insert(code.to_owned());
            render_selected(&self.selected_box, self.kind, &self.selected, &self.checks);
        }
    }

    #[cfg(test)]
    pub(crate) fn deselect(&self, code: &str) {
        let check = self
            .checks
            .borrow()
            .iter()
            .find(|(known, _)| known == code)
            .map(|(_, check)| check.clone());
        if let Some(check) = check {
            check.set_active(false);
        } else {
            self.selected.borrow_mut().remove(code);
            render_selected(&self.selected_box, self.kind, &self.selected, &self.checks);
        }
    }

    #[cfg(test)]
    pub(crate) fn pending_manual(&self, value: &str) {
        if let Some((entry, _)) = &self.manual {
            entry.set_text(value);
        }
    }

    #[cfg(test)]
    pub(crate) fn enter_manual(&self, tag: &str) -> Result<(), String> {
        let code = validate_manual_language(tag)?;
        self.selected.borrow_mut().insert(code);
        Ok(())
    }

    pub(crate) fn focus(&self) {
        self.chooser.set_expanded(true);
        self.chooser.grab_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn country_coverage_names_and_language_tags() {
        assert_eq!(COUNTRY_NAMES.len(), 249);
        assert_eq!(
            human_label(FacetDimension::Country, "DE"),
            "Germany (DE)".to_string().replace(
                "Germany",
                if german_ui() {
                    "Deutschland"
                } else {
                    "Germany"
                }
            )
        );
        assert!(COUNTRY_NAMES.iter().any(|(code, _, _)| *code == "AT"));
        assert!(LANGUAGE_NAMES
            .iter()
            .any(|(code, en, _)| *code == "de" && *en == "German"));
        assert_eq!(validate_manual_language("de-DE").unwrap(), "de-DE");
        assert!(validate_manual_language("German").is_err());
        assert!(normalize_facet("de-DE", FacetDimension::Country).is_err());
        for (code, _, _) in COUNTRY_NAMES {
            assert_eq!(
                normalize_facet(code, FacetDimension::Country).unwrap(),
                *code,
                "all available countries are selectable"
            );
        }
    }
}
