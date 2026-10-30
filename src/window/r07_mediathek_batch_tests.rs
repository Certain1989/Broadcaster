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

// D07: enter the *actual* responsive title/bottom ViewSwitcher from Search
// using XTest Up, activate all three real page buttons with XTest Return,
// and return from each switcher to the page Search with XTest Down.
// The disconnected My Channels result list is not manufactured here: this
// covers the category transition, not the whole 91-ID D07 clause.
fn run_actual_three_category_switcher(width: i32, height: i32, negative: bool) {
    adw::init().expect("native GTK display");
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
    // The required D07 gate covers spinner→loaded as well as loaded page
    // switching, so the previous timing-dependent hidden-focus assertion
    // cannot silently pass only when the external Live feed happens to load.
    check_switcher_loading_boundary(&win);
    // Return to the starting page for the independent three-tab route.
    win.imp().stack.set_visible_child_name("mediathek");
    settle();

    let med = &win.imp().mediathek_view;
    let search = med.toolbar_focus_targets()[0].clone();
    assert!(search.grab_focus(), "R07 D07 initial Search focus");
    settle();
    for (index, (page, title)) in [
        ("live", "Live"),
        ("mediathek", "Mediathek"),
        ("streams", "My Channels"),
    ]
    .into_iter()
    .enumerate()
    {
        let start = win.focused_widget().expect("D07 current Search focus");
        assert!(start.is_mapped() && start.is_sensitive());
        inject("Up", index == 0);
        let switcher = TvWindow::find_descendant(win.upcast_ref(), &|widget| {
            (widget.is::<adw::ViewSwitcher>() || widget.is::<adw::ViewSwitcherBar>())
                && widget.is_mapped()
        })
        .expect("real responsive mapped ViewSwitcher");
        let focused = win.focused_widget().expect("D07 focused real switcher button");
        assert!(
            TvWindow::is_within(&focused, &switcher),
            "D07 {title}: native Up must enter actual page switcher"
        );
        // The switcher initially focuses its currently selected page, not
        // the first tab. Move to the adjacent requested category physically.
        inject(if index == 0 { "Left" } else { "Right" }, false);
        let focused = win.focused_widget().expect("D07 active switcher tab");
        assert!(focused.is_mapped() && TvWindow::is_within(&focused, &switcher));
        inject("Return", false);
        if negative {
            assert_eq!(
                win.imp().stack.visible_child_name().as_deref(),
                Some("wrong-category"),
                "R07_D07_EXPECTED_NEGATIVE_ORACLE: physical selected page identity"
            );
            unreachable!("D07 wrong-page oracle unexpectedly survived");
        }
        assert_eq!(
            win.imp().stack.visible_child_name().as_deref(),
            Some(page),
            "D07 {title}: native switcher Return must change selected page"
        );
        // The switcher remains focused until Down passes through the actual
        // window boundary handler, restoring this page's Search.
        inject("Down", false);
        let expected = match page {
            "live" => win.imp().live_view.toolbar_focus_targets()[0].clone(),
            "mediathek" => win.imp().mediathek_view.toolbar_focus_targets()[0].clone(),
            "streams" => win.imp().streams_view.toolbar_focus_targets()[0].clone(),
            _ => unreachable!(),
        };
        let focused = win.focused_widget().expect("D07 restored page Search owner");
        assert!(
            TvWindow::is_within(&focused, &expected) && focused.is_mapped(),
            "D07 {title}: Down must focus the newly selected page Search"
        );
    }
    // Route back to Mediathek through physical Home, not a direct stack setter.
    inject("XF86HomePage", false);
    assert_eq!(
        win.imp().stack.visible_child_name().as_deref(),
        Some("mediathek"),
        "D07 Home from My Channels must restore Mediathek"
    );
    win.destroy();
}

#[test]
#[ignore = "R07 D07 real responsive three-category native switcher, compact"]
fn r07_native_category_switcher_640_480() {
    run_actual_three_category_switcher(640, 480, false);
}

#[test]
#[ignore = "R07 D07 real responsive three-category native switcher, TV"]
fn r07_native_category_switcher_1280_800() {
    run_actual_three_category_switcher(1280, 800, false);
}

#[test]
#[ignore = "R07 D07 intentionally wrong category after physical switch"]
fn r07_native_category_switcher_negative_page() {
    run_actual_three_category_switcher(640, 480, true);
}

// Bounded native spinner→populated transition. Both the separate loading
// test and the required three-category CI gate call this same physical route.
fn check_switcher_loading_boundary(win: &TvWindow) {
    let med_search = win.imp().mediathek_view.toolbar_focus_targets()[0].clone();
    assert!(med_search.grab_focus(), "D07 loading starts on mapped Mediathek Search");
    settle();
    inject("Up", true);
    let switcher = TvWindow::find_descendant(win.upcast_ref(), &|w| {
        (w.is::<adw::ViewSwitcher>() || w.is::<adw::ViewSwitcherBar>()) && w.is_mapped()
    })
    .expect("mapped responsive view switcher");
    assert!(
        TvWindow::is_within(&win.focused_widget().unwrap(), &switcher),
        "D07 loading route reaches actual switcher"
    );
    inject("Left", false);
    inject("Return", false);
    assert_eq!(win.imp().stack.visible_child_name().as_deref(), Some("live"));
    let live_search = win.imp().live_view.toolbar_focus_targets()[0].clone();
    assert!(
        !live_search.is_mapped(),
        "D07 spinner fixture must not silently become a populated Live page"
    );
    inject("Down", false);
    let owner = win.focused_widget().expect("D07 focus during loading");
    assert!(
        owner.is_mapped() && !TvWindow::is_within(&owner, &live_search),
        "D07 loading must keep focus visible, not assign hidden Live GtkText: {}",
        owner.type_().name()
    );
    assert!(
        TvWindow::is_within(&owner, &switcher),
        "D07 loading Down remains inside mapped page switcher"
    );
    // Complete the real widget's offline load, without a network request.
    win.imp().live_view.show_toolbar_for_test();
    win.imp().live_view.install_r07_native_channel_rows();
    settle();
    assert!(live_search.is_mapped(), "Live Search now visibly ready");
    inject("Down", false);
    let owner = win.focused_widget().expect("D07 focus after loading");
    assert!(
        owner.is_mapped() && TvWindow::is_within(&owner, &live_search),
        "D07 second physical Down reaches mapped Live Search"
    );
    // Page boundary remains reversible after the loading→populated transition.
    inject("Up", false);
    let owner = win.focused_widget().expect("D07 inverse after load");
    assert!(
        owner.is_mapped() && TvWindow::is_within(&owner, &switcher),
        "D07 Up returns to responsive switcher after real load"
    );
}

fn run_switcher_loading_boundary(width: i32, height: i32) {
    adw::init().expect("native GTK X11");
    let resource =
        gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
    gio::resources_register(&resource);
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("registered native application");
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    settle();
    check_switcher_loading_boundary(&win);
    win.destroy();
}

#[test]
#[ignore = "D07 native Live spinner-to-ready focus boundary, compact"]
fn r07_native_category_loading_640_480() {
    run_switcher_loading_boundary(640, 480);
}

#[test]
#[ignore = "D07 native Live spinner-to-ready focus boundary, TV"]
fn r07_native_category_loading_1280_800() {
    run_switcher_loading_boundary(1280, 800);
}

// E04: complete physical first/middle/last Mediathek card route, with native
// Return entering the real Play action and Back returning to the same row.
// Unlike E05's action graph, this also verifies the reverse path to Search.
fn r07_mediathek_three_row_inverse(width: i32, height: i32, negative: bool) {
    adw::init().expect("native display");
    let resource =
        gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
    gio::resources_register(&resource);
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("registered actual application");
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    settle();
    win.imp().stack.set_visible_child_name("mediathek");
    let med = &win.imp().mediathek_view;
    med.r07_install_offline_result_cards();
    settle();
    let search = &med.toolbar_focus_targets()[0];
    assert!(search.grab_focus(), "E04 actual Search entry");
    settle();
    for _ in 0..8 {
        if med.r07_result_row(0).has_focus() {
            break;
        }
        inject("Down", false);
    }
    for index in 0..3 {
        let row = med.r07_result_row(index);
        assert!(row.has_focus(), "E04 native row {index} direct focus");
        assert_eq!(
            med.r07_result_id(index),
            Some(format!("Offline show {index}")),
            "E04 actual source-qualified Mediathek row identity"
        );
        if negative {
            assert_eq!(
                med.r07_result_id(index).as_deref(),
                Some("wrong-show"),
                "R07_E04_EXPECTED_NEGATIVE_ORACLE: native Mediathek row identity"
            );
            unreachable!("E04 deliberately wrong row ID unexpectedly matched");
        }
        inject("Return", false);
        let owner = win.focused_widget().expect("E04 Play focus owner");
        assert_eq!(
            owner.ancestor(gtk::ListBoxRow::static_type()),
            Some(row.clone().upcast::<gtk::Widget>()),
            "E04 action must belong to the same Mediathek row"
        );
        assert!(owner.is_mapped() && owner.is_sensitive());
        inject("XF86Back", false);
        assert!(row.has_focus(), "E04 return to same row after Back");
        if index < 2 {
            inject("Down", false);
        }
    }
    for index in (0..2).rev() {
        inject("Up", false);
        assert!(med.r07_result_row(index).has_focus(), "E04 reverse row {index}");
    }
    // Return from the selected result through the actual mapped toolbar.
    let toolbar = med.toolbar_focus_targets();
    for step in 0..10 {
        let owner = win.focused_widget().expect("E04 reverse owner");
        if TvWindow::is_within(&owner, search) {
            break;
        }
        let top = toolbar.iter().take(4).enumerate().find_map(|(i, widget)| {
            TvWindow::is_within(&owner, widget).then_some(i)
        });
        eprintln!("R07_E04_REV step={step} focused={} top={top:?}", owner.type_().name());
        inject(if top.is_some() { "Left" } else { "Up" }, false);
    }
    assert!(
        TvWindow::is_within(
            &win.focused_widget().expect("E04 final Search owner"),
            search
        ),
        "E04 native reverse must return to mapped Search"
    );
    win.destroy();
}

#[test]
#[ignore = "R07 E04 real three-row Mediathek entry/reverse 640x480"]
fn r07_native_mediathek_three_rows_640_480() {
    r07_mediathek_three_row_inverse(640, 480, false);
}

#[test]
#[ignore = "R07 E04 real three-row Mediathek entry/reverse 1280x800"]
fn r07_native_mediathek_three_rows_1280_800() {
    r07_mediathek_three_row_inverse(1280, 800, false);
}

#[test]
#[ignore = "R07 E04 deliberately wrong real Mediathek row identity"]
fn r07_native_mediathek_three_rows_negative_identity() {
    r07_mediathek_three_row_inverse(640, 480, true);
}
