// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// R07 E05: the exact real Mediathek action row, not four generic Buttons.
// This does not activate any external URI, download or playback action.
fn run_mediathek_card_actions(width: i32, height: i32, negative: bool) {
    adw::init().expect("native display");
    let resource =
        gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
    gio::resources_register(&resource);
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("registered real application");
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    eprintln!("R07 E05 window presented");
    settle();
    win.imp().stack.set_visible_child_name("mediathek");
    let med = &win.imp().mediathek_view;
    med.r07_install_offline_result_cards();
    eprintln!("R07 E05 rows installed");
    settle();

    // One focus seed at the real Search entry. All subsequent entries and
    // action transitions are real injected XTest keys.
    let search = &med.toolbar_focus_targets()[0];
    assert!(search.grab_focus(), "real Mediathek Search focus");
    settle();
    // Traverse all real graphical filters to the first result; don't assume
    // a fixed number of GTK hop keys on compact versus TV layouts.
    for step in 0..8 {
        if med.r07_result_row(0).has_focus() {
            break;
        }
        inject("Down", step == 0);
        let owner = win.focused_widget().expect("E05 Down owner");
        eprintln!("R07 E05 search Down {step}: {}", owner.type_().name());
    }
    for index in 0..3 {
        eprintln!("R07 E05 begin row {index}");
        let row = med.r07_result_row(index);
        assert!(
            row.has_focus(),
            "E05 native Mediathek row {index} must own focus"
        );
        assert_eq!(
            med.r07_result_id(index),
            Some(format!("Offline show {index}"))
        );
        inject("Return", false);
        let play = TvWindow::find_descendant(row.upcast_ref(), &|w| w.is::<adw::SplitButton>())
            .expect("real card's Play split action");
        let copy = TvWindow::find_descendant(row.upcast_ref(), &|w| w.is::<gtk::MenuButton>())
            .expect("real card's Copy menu action");
        let download = TvWindow::find_descendant(row.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>()
                .is_some_and(|button| button.action_name().as_deref() == Some("card.download"))
        })
        .expect("real card's Download action");
        let website = TvWindow::find_descendant(row.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>()
                .is_some_and(|button| button.action_name().as_deref() == Some("card.open-website"))
        })
        .expect("real card's Website action");
        for (name, action) in [
            ("Play", &play),
            ("Copy", &copy),
            ("Download", &download),
            ("Website", &website),
        ] {
            // Split buttons may have two focusable internal children, so allow
            // bounded physical Right presses, never programmatic target seeding.
            let mut found = false;
            for step in 0..6 {
                let owner = win.focused_widget().expect("E05 mapped focus owner");
                if TvWindow::is_within(&owner, action) {
                    assert!(owner.is_mapped() && owner.is_sensitive() && owner.is_visible());
                    assert_eq!(
                        owner.ancestor(gtk::ListBoxRow::static_type()),
                        Some(row.clone().upcast::<gtk::Widget>()),
                        "E05 {name} focus must belong to this source-qualified card"
                    );
                    found = true;
                    break;
                }
                assert!(
                    step < 5,
                    "E05 {name} unreachable after six physical Right keys"
                );
                inject("Right", false);
            }
            assert!(found, "E05 {name} physically unreachable");
        }
        if negative {
            assert_eq!(
                med.r07_result_id(index).as_deref(),
                Some("Offline wrong source"),
                "R07_E05_EXPECTED_NEGATIVE_ORACLE: wrong Mediathek show identity"
            );
            unreachable!("E05 wrong-show oracle unexpectedly survived");
        }
        inject("XF86Back", false);
        assert!(
            row.has_focus(),
            "E05 Back must restore row after action graph"
        );
        if index < 2 {
            inject("Down", false);
        }
    }
    win.destroy();
}

#[test]
#[ignore = "R07 E05 native mapped Mediathek action graph at compact size"]
fn r07_native_mediathek_actions_640_480() {
    run_mediathek_card_actions(640, 480, false);
}

#[test]
#[ignore = "R07 E05 native mapped Mediathek action graph at TV size"]
fn r07_native_mediathek_actions_1280_800() {
    run_mediathek_card_actions(1280, 800, false);
}

#[test]
#[ignore = "R07 E05 intentionally wrong Mediathek source identity"]
fn r07_native_mediathek_actions_negative_source() {
    run_mediathek_card_actions(640, 480, true);
}

// Cross-category Back/Home checks use real window key capture at both sizes.
// Direct page choice here is fixture setup, not proof of full D07 switcher paths.
fn run_home_back_across_pages(width: i32, height: i32) {
    adw::init().expect("native display");
    let resource =
        gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
    gio::resources_register(&resource);
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("registered real application");
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    settle();
    for page in ["live", "streams"] {
        for key in ["XF86Back", "XF86HomePage"] {
            win.imp().stack.set_visible_child_name(page);
            settle();
            let targets = if page == "live" {
                win.imp().live_view.show_toolbar_for_test();
                win.imp().live_view.toolbar_focus_targets()
            } else {
                win.imp().streams_view.toolbar_focus_targets()
            };
            assert!(targets[0].grab_focus(), "R07 {page} Search focus seed");
            settle();
            inject(key, true);
            assert_eq!(
                win.imp().stack.visible_child_name().as_deref(),
                Some("mediathek"),
                "R07 {page} {key} must return to Mediathek"
            );
            let owner = win.focused_widget().expect("R07 Home/Back focus owner");
            assert!(
                TvWindow::is_within(&owner, win.imp().mediathek_view.upcast_ref::<gtk::Widget>()),
                "R07 Home/Back must focus an actual Mediathek control"
            );
            assert!(owner.is_mapped() && owner.is_sensitive());
        }
    }
    win.destroy();
}

#[test]
#[ignore = "R07 cross-page physical Home/Back at compact size"]
fn r07_native_page_home_back_640_480() {
    run_home_back_across_pages(640, 480);
}

#[test]
#[ignore = "R07 cross-page physical Home/Back at TV size"]
fn r07_native_page_home_back_1280_800() {
    run_home_back_across_pages(1280, 800);
}
