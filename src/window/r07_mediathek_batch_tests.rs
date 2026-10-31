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
    win.imp().live_view.show_spinner_for_test();
    settle();
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

// R07 D08/D09: actual application Menu keys, every mapped GtkModelButton,
// and Escape/Back/Home dismissal semantics. No menu item is activated.
fn r07_app_menu_native(width: i32, height: i32, negative: bool) {
    adw::init().expect("native GTK display");
    let resource = gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
    gio::resources_register(&resource);
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>).unwrap();
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    settle();
    win.imp().stack.set_visible_child_name("mediathek");
    settle();
    let search = win.imp().mediathek_view.toolbar_focus_targets()[0].clone();
    assert!(search.grab_focus());
    settle();
    let popover = win.imp().menu_button.popover().expect("real menu-model popover");
    for key in ["Escape", "XF86Back", "XF86HomePage"] {
        assert!(search.grab_focus());
        inject("Menu", false);
        assert!(popover.is_mapped(), "real Menu key must open the app menu");
        fn actions(root: &gtk::Widget) -> Vec<gtk::Widget> {
            let mut out = Vec::new();
            let mut child = root.first_child();
            while let Some(w) = child {
                if w.type_().name() == "GtkModelButton" && w.is_sensitive() && w.is_mapped() {
                    out.push(w.clone());
                }
                out.extend(actions(&w));
                child = w.next_sibling();
            }
            out
        }
        let buttons = actions(popover.upcast_ref());
        assert_eq!(buttons.len(), 6, "the six real enabled app menu actions");
        // Start at first GTK model button; every other step uses a physical key.
        assert!(buttons[0].grab_focus());
        settle();
        let mut visited = Vec::<usize>::new();
        for _ in 0..buttons.len() + 3 {
            if let Some(owner) = win.focused_widget() {
                if let Some((i, _)) = buttons.iter().enumerate().find(|(_, w)| **w == owner) {
                    if !visited.contains(&i) { visited.push(i); }
                }
            }
            inject("Down", false);
        }
        assert_eq!(visited.len(), 6, "every real app menu action must be physically traversable");
        if negative {
            assert_eq!(visited.len(), 7, "R07_D08_EXPECTED_NEGATIVE_ORACLE: phantom seventh action must be rejected");
        }
        for _ in 0..buttons.len() {
            inject("Up", false);
            assert!(popover.is_mapped(), "Up navigation cannot close or escape app menu");
        }
        inject(key, false);
        assert!(!popover.is_mapped(), "{key} must dismiss the actual app menu");
        assert_eq!(win.imp().stack.visible_child_name().as_deref(), Some("mediathek"));
        let owner = win.focused_widget().expect("menu dismissal yields mapped app focus");
        assert!(owner.is_mapped() && owner.is_sensitive(), "{key} restores a valid GTK focus owner");
    }
    eprintln!("R07_D08_D09 real six-item app menu {width}x{height} native XTest, 3 dismissals");
    win.destroy();
}
#[test]
#[ignore = "R07 D08/D09 real native application menu and Escape Back Home compact"]
fn r07_native_app_menu_640_480() { r07_app_menu_native(640,480,false); }
#[test]
#[ignore = "R07 D08/D09 real native application menu and Escape Back Home TV"]
fn r07_native_app_menu_1280_800() { r07_app_menu_native(1280,800,false); }
#[test]
#[ignore = "R07 D08 deliberately false enabled app-menu item count"]
fn r07_native_app_menu_negative() { r07_app_menu_native(640,480,true); }

// R07 D03/D04/D10: one completely connected loaded Mediathek route. Unlike
// focus-grabbing each control independently, the only seeded owner is Search;
// every subsequent filter, populated row, and inverse is a native XTest key.
fn r07_mediathek_loaded_vertical(width: i32, height: i32, negative: bool) {
    adw::init().expect("native display");
    gio::resources_register(
        &gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap(),
    );
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("registered real app");
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    settle();
    win.imp().stack.set_visible_child_name("mediathek");
    let med = &win.imp().mediathek_view;
    med.r07_install_offline_result_cards();
    settle();
    let toolbar = med.toolbar_focus_targets();
    assert_eq!(toolbar.len(), 7, "known real Mediathek upper controls");
    assert!(toolbar[0].grab_focus(), "initial Search native focus seed");
    settle();
    let focused_in = |target: &gtk::Widget, context: &str| {
        let owner = win.focused_widget().expect("D03/D04 live window owner");
        let found = toolbar.iter().position(|w| TvWindow::is_within(&owner, w));
        eprintln!("R07_D03_GRAPH {width}x{height} expected={context} mapped_index={found:?} owner={} target={}", owner.type_().name(), target.type_().name());
        assert!(
            owner.is_mapped() && owner.is_visible() && owner.is_sensitive()
                && TvWindow::is_within(&owner, target),
            "D03/D04 {context}: wrong or invisible GTK target {}",
            owner.type_().name()
        );
    };
    // The keyboard launcher is horizontally adjacent to Search; the filters
    // are the next visual row, not a fixed Down-only sequence. Test both
    // spatial entrances and their actual opposite-key inverses.
    inject("Right", true);
    focused_in(&toolbar[1], "Search Right enters Keyboard");
    inject("Left", false);
    focused_in(&toolbar[0], "Keyboard Left restores Search");
    inject("Down", false);
    focused_in(&toolbar[4], "Search Down reaches Broadcaster");
    inject("Right", false);
    focused_in(&toolbar[5], "Broadcaster Right reaches Duration");
    inject("Right", false);
    focused_in(&toolbar[6], "Duration Right reaches Sort");
    for i in 0..3 {
        inject("Down", false);
        let row = med.r07_result_row(i);
        focused_in(row.upcast_ref(), "loaded Mediathek row");
        assert_eq!(
            med.r07_result_id(i),
            Some(format!("Offline show {i}")),
            "source-qualified fetched-result identity survives physical traversal"
        );
    }
    if negative {
        assert_eq!(
            med.r07_result_id(2).as_deref(),
            Some("Offline wrong show"),
            "R07_D03_EXPECTED_NEGATIVE_ORACLE: final loaded-row identity"
        );
        unreachable!("D03 wrong-row oracle unexpectedly survived");
    }
    // An expanded result's primary Play action must reverse to its exact
    // loaded row using Back, with no backend/media network activation.
    inject("Return", false);
    let last = med.r07_result_row(2);
    let play = TvWindow::find_descendant(last.upcast_ref(), &|w| w.is::<adw::SplitButton>())
        .expect("actual loaded row's Play action");
    focused_in(&play, "last-row Play after Return");
    inject("XF86Back", false);
    focused_in(last.upcast_ref(), "last-row Back inverse");

    for index in (0..=1).rev() {
        inject("Up", false);
        focused_in(med.r07_result_row(index).upcast_ref(), "physical previous row");
    }
    inject("Up", false);
    focused_in(&toolbar[6], "results Up returns to Sort");
    // D09/D10: Back dismisses the *actual* Sort popover; Up while the
    // menu is open stays inside that native menu rather than escaping to
    // toolbar/results. No sort action or remote service is triggered.
    let sort = toolbar[6]
        .clone()
        .downcast::<gtk::MenuButton>()
        .expect("native Mediathek Sort menu button");
    let popup = sort.popover().expect("real Sort popover");
    inject("Return", false);
    assert!(popup.is_mapped(), "Sort Return opens actual GTK popover");
    inject("Up", false);
    assert!(popup.is_mapped(), "Up inside Sort popup must not escape");
    inject("XF86Back", false);
    assert!(!popup.is_mapped(), "Back closes Sort popup before leaving page");
    focused_in(&toolbar[6], "Sort Back restores same toolbar action");
    inject("Left", false);
    focused_in(&toolbar[5], "Sort Left restores Duration");
    inject("Left", false);
    focused_in(&toolbar[4], "Duration Left restores Broadcaster");
    inject("Up", false);
    focused_in(&toolbar[1], "Broadcaster Up reaches upper Keyboard");
    inject("Left", false);
    focused_in(&toolbar[0], "Keyboard Left returns Search");
    eprintln!(
        "R07_D03_D04_D10 {width}x{height}: populated physical Search->filters->3 results->Play/Back->Sort/Duration->Search inverse PASS"
    );
    win.destroy();
}

#[test]
#[ignore = "R07 native full Mediathek loaded header/filters/results graph compact"]
fn r07_native_mediathek_loaded_vertical_640_480() {
    r07_mediathek_loaded_vertical(640, 480, false);
}

#[test]
#[ignore = "R07 native full Mediathek loaded header/filters/results graph TV"]
fn r07_native_mediathek_loaded_vertical_1280_800() {
    r07_mediathek_loaded_vertical(1280, 800, false);
}

#[test]
#[ignore = "R07 deliberately wrong Mediathek loaded final-row identity"]
fn r07_native_mediathek_loaded_vertical_negative() {
    r07_mediathek_loaded_vertical(640, 480, true);
}

// E03: the real Live row's first OK enters Play without starting playback;
// second OK goes through the real application action and its explicitly
// disposable failed managed-player boundary. The test-only backend performs
// no URL fetch, system-player launch or alteration of a real playlist.
fn r07_live_player_failed_return(width: i32, height: i32, negative: bool) {
    adw::init().expect("native GTK X11 display");
    let resource = gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
        .expect("real application resources");
    gio::resources_register(&resource);
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("native registered application");
    app.r07_test_managed_player_failure(true);
    // app.window() deliberately caches this exact TvWindow, because the
    // managed-player failure path restores focus on the application window.
    let win = app.window();
    win.set_default_size(width, height);
    win.present();
    settle();
    win.imp().stack.set_visible_child_name("live");
    let live = &win.imp().live_view;
    live.show_toolbar_for_test();
    live.install_r07_native_channel_rows();
    settle();
    let origin = live.r07_native_channel_id(0).expect("first real offline Live row");
    assert!(live.toolbar_focus_targets()[0].grab_focus());
    settle();
    inject("Down", true);
    let list = TvWindow::find_descendant(live.upcast_ref(), &|w| w.is::<gtk::ListBox>())
        .and_then(|w| w.downcast::<gtk::ListBox>().ok())
        .expect("mapped Live native ListBox");
    let row = list.row_at_index(0).expect("first mapped Live channel card");
    assert!(row.has_focus(), "physical Down enters first Live card");
    assert_eq!(live.r07_native_channel_id(0).as_deref(), Some(origin.as_str()));
    inject("Return", false);
    let play = win.focused_widget().expect("first OK should focus real Live Play");
    assert_eq!(
        play.downcast_ref::<gtk::Button>().and_then(|b| b.action_name()).as_deref(),
        Some("card.play"),
        "first OK focuses the primary Play action"
    );
    assert_eq!(app.r07_test_managed_player_attempts(), 0, "first OK cannot launch player");
    inject("Return", false);
    for _ in 0..40 {
        settle();
        if app.r07_test_managed_player_attempts() == 1 { break; }
    }
    assert_eq!(
        app.r07_test_managed_player_attempts(), 1,
        "second physical OK must enter exactly one fake managed-player failure"
    );
    if negative {
        assert_eq!(
            app.r07_test_managed_player_attempts(), 2,
            "R07_E03_EXPECTED_NEGATIVE_ORACLE: a single second OK cannot start two players"
        );
    }
    assert_eq!(win.imp().stack.visible_child_name().as_deref(), Some("live"));
    let restored = win.focused_widget().expect("failed player restores Live focus");
    assert_eq!(restored, play, "failed player must return to exact focused Play action");
    assert!(restored.is_mapped() && restored.is_sensitive());
    inject("XF86Back", false);
    assert!(row.has_focus(), "Back after failed playback returns to exact Live row");
    assert_eq!(live.r07_native_channel_id(0).as_deref(), Some(origin.as_str()));
    eprintln!("R07_E03_NATIVE {width}x{height} first OK -> Play; second OK -> fake failure -> exact Play/row return");
    win.destroy();
}

#[test]
#[ignore = "R07 Live real second OK failed managed-player focus return 640x480"]
fn r07_native_live_player_failure_640_480() {
    r07_live_player_failed_return(640, 480, false);
}

#[test]
#[ignore = "R07 Live real second OK failed managed-player focus return 1280x800"]
fn r07_native_live_player_failure_1280_800() {
    r07_live_player_failed_return(1280, 800, false);
}

#[test]
#[ignore = "R07 intentionally incorrect double invocation for one second OK"]
fn r07_native_live_player_failure_negative() {
    r07_live_player_failed_return(640, 480, true);
}

// E06: a real enabled Mediathek Load More action submits an asynchronous
// disposable page, retires its own footer on the final page and restores
// source-qualified row focus. No provider URL, stream or mpv is touched.
fn r07_mediathek_paged_focus(width: i32, height: i32, negative: bool) {
    adw::init().expect("native GTK X11 display");
    gio::resources_register(
        &gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
            .expect("application resources"),
    );
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("native registered application");
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    settle();
    win.imp().stack.set_visible_child_name("mediathek");
    let med = &win.imp().mediathek_view;
    med.r07_install_offline_paging();
    settle();
    let toolbar = med.toolbar_focus_targets();
    assert!(toolbar[6].grab_focus(), "physical Sort toolbar portal");
    settle();
    for index in 0..3 {
        inject("Down", false);
        let row = med.r07_result_row(index);
        let owner = win.focused_widget().expect("mapped row focus");
        assert!(TvWindow::is_within(&owner, row.upcast_ref()), "physical Down selects the right row {index}");
        assert_eq!(med.r07_result_id(index).as_deref(), Some(format!("Offline show {index}").as_str()));
    }
    inject("Down", false);
    let more = med.r07_load_more_button();
    let owner = win.focused_widget().expect("mapped Load More owner");
    assert_eq!(owner, more.clone().upcast::<gtk::Widget>(), "Down reaches enabled real Load More");
    assert!(more.is_mapped() && more.is_sensitive());
    inject("Return", false);
    for _ in 0..70 {
        settle();
        if med.r07_offline_result_count() == 6 { break; }
    }
    assert_eq!(med.r07_offline_result_count(), 6, "real Load More asynchronous page appended exactly three rows");
    assert!(!more.is_mapped() || !more.is_sensitive(), "final-page Load More must disappear or disable");
    let new_first = med.r07_result_row(3);
    let owner = win.focused_widget().expect("Load More fallback has real focus");
    assert!(new_first.has_focus() && TvWindow::is_within(&owner, new_first.upcast_ref()),
        "async final-page focus transfers directly to first new result");
    assert_eq!(med.r07_result_id(3).as_deref(), Some("Offline show 3"));
    if negative {
        assert_eq!(med.r07_result_id(3).as_deref(), Some("Wrong show"),
            "R07_E06_EXPECTED_NEGATIVE_ORACLE: wrong new-page identity must fail");
    }
    inject("Return", false);
    let owner = win.focused_widget().expect("new Mediathek row opens Play");
    let play = TvWindow::find_descendant(new_first.upcast_ref(), &|w| w.is::<adw::SplitButton>())
        .expect("newly paginated card's real Play composite");
    assert!(TvWindow::is_within(&owner, &play),
        "OK on new result must enter Play or its focusable split-button child");
    inject("XF86Back", false);
    let owner = win.focused_widget().expect("Back restores selected new row");
    assert!(TvWindow::is_within(&owner, new_first.upcast_ref()), "Back returns to new result, not stale Load More");
    // The new result's real action group includes native menu popovers.
    // The same physical control graph must let Back dismiss the selected
    // quality/copy menu without leaving the paged row or activating a URL.
    inject("Return", false);
    // AdwSplitButton itself contains a GtkMenuButton for its quality arrow.
    // Select the *separate* Copy URL action, not that internal Play control.
    let copy = TvWindow::find_descendant(new_first.upcast_ref(), &|widget| {
        widget.is::<gtk::MenuButton>()
            && widget.ancestor(adw::SplitButton::static_type()).is_none()
    })
    .and_then(|widget| widget.downcast::<gtk::MenuButton>().ok())
    .expect("real paginated Copy Video URL menu button");
    let mut reached_copy = false;
    for _ in 0..5 {
        let owner = win.focused_widget().expect("E06 focused native card action");
        if TvWindow::is_within(&owner, copy.upcast_ref()) {
            reached_copy = true;
            break;
        }
        inject("Right", false);
    }
    assert!(reached_copy, "E06 real paged result Copy menu must be D-pad reachable");
    inject("Return", false);
    let popover = copy.popover().expect("real copy-quality menu popover");
    assert!(popover.is_mapped(), "physical OK opens the real card menu");
    inject("XF86Back", false);
    assert!(!popover.is_mapped(), "physical Back dismisses only the card menu");
    let owner = win.focused_widget().expect("E06 focus after card menu dismissal");
    assert!(
        TvWindow::is_within(&owner, copy.upcast_ref()),
        "E06 nested menu Back must return to exact Copy opener"
    );
    inject("XF86Back", false);
    assert!(
        TvWindow::is_within(&win.focused_widget().expect("E06 final row restoration"), new_first.upcast_ref()),
        "E06 second Back restores same paged card"
    );
    eprintln!("R07_E06 {width}x{height} native 3+3 paging, Copy URL menu/Back, exact row restoration PASS");
    win.destroy();
}

#[test]
#[ignore = "R07 E06 real disposable Mediathek Load More and async focus 640x480"]
fn r07_native_mediathek_paged_640_480() { r07_mediathek_paged_focus(640, 480, false); }
#[test]
#[ignore = "R07 E06 real disposable Mediathek Load More and async focus 1280x800"]
fn r07_native_mediathek_paged_1280_800() { r07_mediathek_paged_focus(1280, 800, false); }
#[test]
#[ignore = "R07 E06 intentionally false newly loaded result identity"]
fn r07_native_mediathek_paged_negative() { r07_mediathek_paged_focus(640, 480, true); }

// E07: the app's real Reload action is exercised through its actual Retry
// widget after a disposable asynchronous failed request, with no provider
// traffic. This is distinct from simply manually focusing a fake Retry button.
fn r07_mediathek_error_retry_focus(width: i32, height: i32, negative: bool) {
    adw::init().expect("native X11 GTK display");
    gio::resources_register(
        &gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
            .expect("real compiled resources"),
    );
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>).expect("registered app");
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    settle();
    win.imp().stack.set_visible_child_name("mediathek");
    let med = &win.imp().mediathek_view;
    let search = med.toolbar_focus_targets()[0].clone();
    assert!(search.grab_focus());
    settle();
    assert_eq!(med.r07_offline_result_count(), 0, "initial empty Mediathek result list");
    assert!(med.r07_offline_initial_empty_is_mapped(),
        "empty-query status page paints before the first request");
    // No results must not trap D-pad navigation in an invisible list.
    inject("Down", false);
    let owner = win.focused_widget().expect("empty-state D-pad owner");
    assert!(owner.is_mapped() && TvWindow::is_within(&owner, med.upcast_ref()),
        "empty state routes to a mapped Mediathek control");
    assert!(search.grab_focus(), "empty state Search inverse remains available");
    med.r07_enable_offline_error_retry();
    // Observe real pending spinner before the fake transport's timer resolves;
    // settle() would intentionally advance past the loading frame.
    for _ in 0..50 {
        if med.r07_offline_loading_is_mapped() { break; }
        glib::MainContext::default().iteration(false);
    }
    assert!(med.r07_offline_loading_is_mapped(), "real loading page must be mapped during pending request");
    for _ in 0..60 {
        settle();
        if med.r07_offline_error_is_mapped() { break; }
    }
    assert!(med.r07_offline_error_is_mapped(), "disposable failed request must show real error/retry page");
    assert_eq!(med.r07_offline_search_calls(), 1, "exactly one initial failed request");
    let retry = TvWindow::find_descendant(med.upcast_ref(), &|w| {
        w.downcast_ref::<gtk::Button>().is_some_and(|b| b.label().as_deref() == Some("Retry"))
    }).expect("real mapped Retry action");
    assert!(retry.is_mapped() && retry.is_sensitive(), "Retry must be enabled and painted");
    let sort = med.toolbar_focus_targets()[6].clone();
    assert!(sort.grab_focus(), "start error-page D-pad from mapped Sort control");
    settle();
    let mut reached_retry = false;
    for _ in 0..5 {
        inject("Down", false);
        let owner = win.focused_widget().expect("error/retry D-pad owner");
        assert!(owner.is_mapped() && owner.is_sensitive(), "no hidden focus on error page");
        if TvWindow::is_within(&owner, &retry) {
            reached_retry = true;
            break;
        }
    }
    assert!(reached_retry, "physical Down from toolbar reaches actual Retry without pointer/Tab");
    inject("Return", false);
    for _ in 0..70 {
        settle();
        if med.r07_offline_result_count() == 3 { break; }
    }
    assert_eq!(med.r07_offline_result_count(), 3, "native Retry must complete synthetic successful search");
    assert_eq!(med.r07_offline_search_calls(), 2, "exactly one physical Retry request");
    if negative {
        assert_eq!(med.r07_offline_search_calls(), 3,
            "R07_E07_EXPECTED_NEGATIVE_ORACLE: extra phantom Retry request must fail");
    }
    assert!(!med.r07_offline_error_is_mapped(), "recovered results replace previous error page");
    let owner = win.focused_widget().expect("post-retry meaningful focus owner");
    assert!(owner.is_mapped() && owner.is_sensitive() && TvWindow::is_within(&owner, med.upcast_ref()),
        "retry dismissal may not strand keyboard focus on an off-page button");
    assert_eq!(med.r07_result_id(0).as_deref(), Some("Offline show 0"));
    eprintln!("R07_E07 {width}x{height}: actual error page native Retry, disposable async success and mapped focus PASS");
    win.destroy();
}

#[test]
#[ignore = "R07 E07 real retry error/loading/async focus compact"]
fn r07_native_mediathek_retry_640_480() { r07_mediathek_error_retry_focus(640,480,false); }
#[test]
#[ignore = "R07 E07 real retry error/loading/async focus TV"]
fn r07_native_mediathek_retry_1280_800() { r07_mediathek_error_retry_focus(1280,800,false); }
#[test]
#[ignore = "R07 E07 deliberate phantom third retry rejected"]
fn r07_native_mediathek_retry_negative() { r07_mediathek_error_retry_focus(640,480,true); }

// E11: while a managed fullscreen player owns input, its Back key must not
// navigate the underlying application.  This uses the production player
// return-token lifecycle with a real focused Live Play control, but a
// disposable fake player boundary: no mpv process or provider URL is used.
fn r07_live_fake_fullscreen_cancel(width: i32, height: i32, negative: bool) {
    adw::init().expect("native GTK X11 display");
    gio::resources_register(
        &gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
            .expect("compiled resources"),
    );
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("registered native application");
    let win = app.window();
    win.set_default_size(width, height);
    win.present();
    settle();
    win.imp().stack.set_visible_child_name("live");
    let live = &win.imp().live_view;
    live.show_toolbar_for_test();
    live.install_r07_native_channel_rows();
    settle();
    let search = live.toolbar_focus_targets()[0].clone();
    assert!(search.grab_focus(), "real Live search portal");
    settle();
    inject("Down", true);
    let list = TvWindow::find_descendant(live.upcast_ref(), &|w| w.is::<gtk::ListBox>())
        .and_then(|w| w.downcast::<gtk::ListBox>().ok())
        .expect("real Live card list");
    let row = list.row_at_index(0).expect("first real Live row");
    assert!(row.has_focus(), "physical Down enters source-qualified card");
    assert_eq!(live.r07_native_channel_id(0).as_deref(), Some("r07-live-0"));
    inject("Return", false);
    let play = win.focused_widget().expect("real focused Play");
    assert_eq!(
        play.downcast_ref::<gtk::Button>()
            .and_then(|button| button.action_name())
            .as_deref(),
        Some("card.play"),
        "first physical OK focuses Play, it does not start playback"
    );
    // The active fake player reserves an actual production return token.
    // Its key capture remains real: no direct call to the window key router.
    let token = win.begin_playback();
    inject("XF86Back", false);
    assert_eq!(
        win.imp().stack.visible_child_name().as_deref(),
        Some("live"),
        "fullscreen Back cannot navigate the underlying Live page"
    );
    assert_eq!(
        win.focused_widget().as_ref(),
        Some(&play),
        "underlying Play focus remains unchanged while player owns input"
    );
    // A cancelled player finishing asynchronously must restore the same
    // source-qualified Play target, not a different card or the toolbar.
    win.finish_playback(token);
    let restored = win.focused_widget().expect("post-cancel Play focus");
    assert_eq!(restored, play, "player cancel restores exact Play control");
    assert!(restored.is_mapped() && restored.is_sensitive());
    if negative {
        assert!(
            row.has_focus(),
            "R07_E11_EXPECTED_NEGATIVE_ORACLE: wrong focus after player cancellation"
        );
    }
    // The same Back key, now after the player exited, must reverse into
    // the exact owning Live row, rather than exiting the page.
    inject("XF86Back", false);
    assert!(row.has_focus(), "Back after cancellation returns to Live row");
    assert_eq!(live.r07_native_channel_id(0).as_deref(), Some("r07-live-0"));

    // Race canary: an old cancelled player's callback must not consume the
    // return snapshot for a replacement player, including when the newer
    // window has changed page intentionally.
    assert!(play.grab_focus(), "re-enter real Play for replacement canary");
    let old = win.begin_playback();
    let current = win.begin_playback();
    win.finish_playback(old);
    assert_eq!(win.focused_widget().as_ref(), Some(&play));
    win.finish_playback(current);
    assert_eq!(win.focused_widget().as_ref(), Some(&play));
    eprintln!("R07_E11_NATIVE {width}x{height}: fake fullscreen Back swallowed, cancel and stale watcher preserve exact Play/row");
    win.destroy();
}

#[test]
#[ignore = "R07 E11 native fake-fullscreen cancellation and focus at compact size"]
fn r07_native_fake_fullscreen_cancel_640_480() {
    r07_live_fake_fullscreen_cancel(640, 480, false);
}

#[test]
#[ignore = "R07 E11 native fake-fullscreen cancellation and focus at TV size"]
fn r07_native_fake_fullscreen_cancel_1280_800() {
    r07_live_fake_fullscreen_cancel(1280, 800, false);
}

#[test]
#[ignore = "R07 E11 deliberately wrong focus owner on fake-player cancellation"]
fn r07_native_fake_fullscreen_cancel_negative() {
    r07_live_fake_fullscreen_cancel(640, 480, true);
}

// D07: one connected native journey, not three disconnected per-page canaries.
// Enter each page through the actual responsive ViewSwitcher, exercise its
// populated real card or virtual row and reverse to the page's toolbar.
// Uses only test-only offline channel data; no player or provider is invoked.
fn r07_three_page_loaded_action_roundtrip(width: i32, height: i32, negative: bool) {
    adw::init().expect("GTK X11 native display");
    gio::resources_register(
        &gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
            .expect("compiled real resources"),
    );
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("registered test application");
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    settle();
    win.imp().stack.set_visible_child_name("mediathek");
    win.imp().mediathek_view.r07_install_offline_result_cards();
    settle();
    assert!(win.imp().mediathek_view.toolbar_focus_targets()[0].grab_focus());
    settle();

    for (index, page) in ["live", "mediathek", "streams"].into_iter().enumerate() {
        inject("Up", index == 0);
        let switcher = TvWindow::find_descendant(win.upcast_ref(), &|widget| {
            (widget.is::<adw::ViewSwitcher>() || widget.is::<adw::ViewSwitcherBar>())
                && widget.is_mapped()
        })
        .expect("mapped real ViewSwitcher");
        assert!(
            TvWindow::is_within(&win.focused_widget().expect("mapped switcher focus"), &switcher),
            "native Up enters the current responsive page switcher"
        );
        inject(if index == 0 { "Left" } else { "Right" }, false);
        inject("Return", false);
        assert_eq!(win.imp().stack.visible_child_name().as_deref(), Some(page));
        match page {
            "live" => {
                let live = &win.imp().live_view;
                live.show_toolbar_for_test();
                live.install_r07_native_channel_rows();
            }
            "mediathek" => win.imp().mediathek_view.r07_install_offline_result_cards(),
            "streams" => {
                win.imp().streams_view.install_remote_action_test_rows();
            }
            _ => unreachable!(),
        }
        settle();
        inject("Down", false);
        let toolbar = match page {
            "live" => win.imp().live_view.toolbar_focus_targets(),
            "mediathek" => win.imp().mediathek_view.toolbar_focus_targets(),
            _ => win.imp().streams_view.toolbar_focus_targets(),
        };
        let search = toolbar[0].clone();
        let owner = win.focused_widget().expect("new page Search owner");
        assert!(owner.is_mapped() && TvWindow::is_within(&owner, &search));
        // Physical Left/Right cannot cross back to a different category.
        inject("Right", false);
        let adjacent = win.focused_widget().expect("adjacent toolbar control");
        assert!(adjacent.is_mapped() && adjacent.is_sensitive());
        assert_eq!(win.imp().stack.visible_child_name().as_deref(), Some(page));
        inject("Left", false);
        assert!(TvWindow::is_within(
            &win.focused_widget().expect("search inverse"), &search
        ));

        // Move only with real Down keys until populated content becomes the
        // focus owner. The top bar has different spatial geometry per page.
        let mut reached = false;
        for _ in 0..16 {
            inject("Down", false);
            let owner = win.focused_widget().expect("D07 downward focus owner");
            assert!(owner.is_mapped() && owner.is_sensitive());
            reached = match page {
                "live" => owner.is::<gtk::ListBoxRow>(),
                "mediathek" => owner.is::<gtk::ListBoxRow>(),
                "streams" => win.imp().streams_view.contains_results_focus(&owner),
                _ => unreachable!(),
            };
            if reached { break; }
        }
        assert!(reached, "D07 native Down must reach populated {page} result");
        let row_owner = win.focused_widget().expect("first result owner");
        let expected_id = match page {
            "live" => {
                assert_eq!(win.imp().live_view.r07_native_channel_id(0).as_deref(), Some("r07-live-0"));
                "r07-live-0".to_owned()
            }
            "mediathek" => {
                assert_eq!(win.imp().mediathek_view.r07_result_id(0).as_deref(), Some("Offline show 0"));
                "Offline show 0".to_owned()
            }
            "streams" => win.imp().streams_view.remote_action_test_selected_id().expect("selected source ID"),
            _ => unreachable!(),
        };
        if negative && page == "streams" {
            assert_eq!(expected_id, "wrong-source", "R07_D07_CONNECTED_EXPECTED_NEGATIVE: wrong final page row");
        }
        inject("Return", false);
        let action = win.focused_widget().expect("result OK primary action");
        assert!(action.is_mapped() && action.is_sensitive(), "real OK target must be mapped");
        assert_eq!(win.imp().stack.visible_child_name().as_deref(), Some(page));
        inject("XF86Back", false);
        let returned = win.focused_widget().expect("real card Back owner");
        assert!(returned.is_mapped() && returned.is_sensitive());
        assert!(
            page == "streams" && win.imp().streams_view.contains_results_focus(&returned)
                || page != "streams" && returned == row_owner,
            "D07 Back restores correct page row rather than a detached/other-page widget"
        );
        // Restore the current page's Search as the starting point for the
        // next native page-switcher journey. This seed is only between pages.
        assert!(search.grab_focus());
        settle();
        eprintln!("R07_D07_CONNECTED {width}x{height}: {page} physical switch, Search inverse, result OK/Back, {expected_id}");
    }
    inject("XF86HomePage", false);
    assert_eq!(win.imp().stack.visible_child_name().as_deref(), Some("mediathek"));
    win.destroy();
}

#[test]
#[ignore = "R07 D07 three connected loaded categories compact XTest"]
fn r07_native_three_page_loaded_640_480() {
    r07_three_page_loaded_action_roundtrip(640, 480, false);
}

#[test]
#[ignore = "R07 D07 three connected loaded categories TV XTest"]
fn r07_native_three_page_loaded_1280_800() {
    r07_three_page_loaded_action_roundtrip(1280, 800, false);
}

#[test]
#[ignore = "R07 D07 wrong final source identity must fail"]
fn r07_native_three_page_loaded_negative() {
    r07_three_page_loaded_action_roundtrip(640, 480, true);
}
