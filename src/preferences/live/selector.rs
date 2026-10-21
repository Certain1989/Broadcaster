// SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org>
// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

#[cfg(test)]
thread_local! {
    static SUPPRESS_LIVE_LOAD: Cell<bool> = const { Cell::new(false) };
}

use adw::{gdk, gio, glib, gtk, prelude::*, subclass::prelude::*};
use indexmap::IndexMap;
use smart_default::SmartDefault;

use crate::{
    application::TvApplication,
    settings::TvSettings,
    zapp::{ChannelId, ChannelInfo},
};

use super::selector_row::TvLiveChannelSelectorRow;

/// Attach a remote handler to the actual menu popover, which is created
/// after the top-level window scans its initial widget tree. Native menu
/// arrows, Return and Escape remain GTK-owned.
pub(super) fn install_channel_menu_remote_handler(button: &gtk::MenuButton) {
    let Some(popover) = button.popover() else {
        return;
    };

    // GTK's popdown can hand focus back after our event handler runs.
    // Restore the origin only after the popover has finished closing.
    let restore_focus = std::rc::Rc::new(Cell::new(false));
    let weak_button = button.downgrade();
    popover.connect_closed({
        let restore_focus = restore_focus.clone();
        move |_| {
            if restore_focus.replace(false) {
                let weak_button = weak_button.clone();
                glib::idle_add_local_once(move || {
                    if let Some(button) = weak_button.upgrade() {
                        if button.is_mapped()
                            && button.is_sensitive()
                            && !button.popover().is_some_and(|menu| menu.is_mapped())
                        {
                            button.grab_focus();
                        }
                    }
                });
            }
        }
    });

    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_button = button.downgrade();
    controller.connect_key_pressed(move |_, key, _, modifiers| {
        if !crate::keyboard::unmodified_remote(modifiers)
            || !matches!(key, gdk::Key::Back | gdk::Key::HomePage)
        {
            return glib::Propagation::Proceed;
        }
        let Some(button) = weak_button.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if !button.popover().is_some_and(|menu| menu.is_mapped()) {
            return glib::Propagation::Proceed;
        }

        // Resolve the modal before closing the menu and releasing its grab.
        let dialog = button
            .ancestor(adw::Dialog::static_type())
            .and_then(|widget| widget.downcast::<adw::Dialog>().ok());
        let root = button.root();
        if key == gdk::Key::Back {
            restore_focus.set(true);
            button.popdown();
        } else {
            restore_focus.set(false);
            button.popdown();
            if let Some(dialog) = dialog {
                dialog.close();
            }
            if let Some(window) =
                root.and_then(|root| root.downcast::<crate::window::TvWindow>().ok())
            {
                // Existing window action and Mediathek focus policy; no global
                // router edits, no navigation behind the still-open modal.
                let _ = gtk::prelude::WidgetExt::activate_action(
                    &window,
                    "window.show-mediathek",
                    None,
                );
                fn focus_browse(widget: &gtk::Widget) -> bool {
                    if let Some(view) = widget.downcast_ref::<crate::mediathek::TvMediathekView>() {
                        view.focus_browse();
                        return true;
                    }
                    let mut child = widget.first_child();
                    while let Some(next) = child {
                        if focus_browse(&next) {
                            return true;
                        }
                        child = next.next_sibling();
                    }
                    false
                }
                focus_browse(window.upcast_ref());
            }
        }
        glib::Propagation::Stop
    });
    popover.add_controller(controller);
}

mod imp {
    use crate::utils::{show_error, spawn, ListStoreExtManual};

    use super::*;

    #[derive(Debug, SmartDefault, gtk::CompositeTemplate)]
    #[template(file = "src/preferences/live/selector.blp")]
    pub struct TvLiveChannelSelector {
        #[template_child]
        pub(super) visible_channel_rows: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub(super) sort_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(super) hidden_channel_rows: TemplateChild<gtk::ListBox>,

        #[default(gio::ListStore::new::<TvLiveChannelSelectorRow>())]
        pub(super) visible_channels: gio::ListStore,
        #[default(gio::ListStore::new::<TvLiveChannelSelectorRow>())]
        pub(super) hidden_channels: gio::ListStore,
        pub(super) channel_rows: RefCell<IndexMap<ChannelId, TvLiveChannelSelectorRow>>,

        pub(super) settings: TvSettings,
        // Initial model population is not an intentional settings change.
        pub(super) loading: Cell<bool>,
    }

    impl TvLiveChannelSelector {
        async fn load(&self) {
            let live_channels = match TvApplication::get().live_channels().await {
                Ok(live_channels) => live_channels,
                Err(_) => {
                    show_error(&gettextrs::gettext(
                        "Failed to load livestream channel settings",
                    ));
                    return;
                }
            };
            let mut visible_channels = self.settings.live_channels();

            self.channel_rows
                .borrow_mut()
                .extend(live_channels.iter().map(|(id, ChannelInfo { name, .. })| {
                    let row = TvLiveChannelSelectorRow::new(id, name);
                    row.connect_visible_notify(glib::clone!(
                        #[weak(rename_to = slf)]
                        self,
                        move |row: &TvLiveChannelSelectorRow| {
                            slf.remove_row(row);

                            if row.visible() {
                                slf.visible_channels.append(row);
                            } else {
                                let channel_rows = slf.channel_rows.borrow();
                                slf.hidden_channels
                                    .typed_insert_sorted::<TvLiveChannelSelectorRow>(
                                        row,
                                        |a, b| {
                                            channel_rows
                                                .get_index_of(&a.channel_id())
                                                .cmp(&channel_rows.get_index_of(&b.channel_id()))
                                        },
                                    );
                            }
                        }
                    ));
                    row.connect_received_drop(glib::clone!(
                        #[weak(rename_to = slf)]
                        self,
                        move |target_row, source_row| {
                            let target_visible = target_row.visible();
                            let source_visible = source_row.visible();

                            if target_visible && source_visible {
                                let src_pos = slf.visible_channels.find(source_row).unwrap();
                                let target_pos = slf.visible_channels.find(target_row).unwrap();

                                slf.visible_channels.remove(src_pos);
                                slf.visible_channels.insert(target_pos, source_row);
                            } else {
                                let target_rows = if target_row.visible() {
                                    source_row.set_visible(true);
                                    &slf.visible_channels
                                } else {
                                    source_row.set_visible(false);
                                    &slf.hidden_channels
                                };
                                slf.remove_row(source_row);

                                let target_pos = target_rows.find(target_row).unwrap();
                                target_rows.insert(target_pos, source_row)
                            }
                        }
                    ));
                    row.connect_move_requested(glib::clone!(
                        #[weak(rename_to = slf)]
                        self,
                        move |row, offset| slf.move_row(row, offset)
                    ));
                    (id.clone(), row)
                }));

            if visible_channels.is_empty() {
                visible_channels.extend(live_channels.keys().cloned());
            }

            let channel_rows = self.channel_rows.borrow();

            for id in &visible_channels {
                if let Some(row) = channel_rows.get(id) {
                    row.set_visible(true);
                }
            }

            for (id, row) in channel_rows.iter() {
                if !visible_channels.contains(id) {
                    row.set_visible(false);
                }
            }
            self.loading.set(false);
        }
        /// Moves a visible row up or down by `offset` positions.
        fn move_row(&self, row: &TvLiveChannelSelectorRow, offset: i32) {
            let Some(position) = self.visible_channels.find(row) else {
                return;
            };
            let Some(new_position) = position
                .checked_add_signed(offset)
                .filter(|&new_position| new_position < self.visible_channels.n_items())
            else {
                return;
            };

            // A single user reorder must not persist its transient removal.
            let was_loading = self.loading.replace(true);
            self.visible_channels.remove(position);
            self.visible_channels.insert(new_position, row);
            self.loading.set(was_loading);
            if !was_loading {
                self.settings.set_live_channels(
                    self.visible_channels
                        .iter::<TvLiveChannelSelectorRow>()
                        .map(|entry| entry.expect("live row").channel_id())
                        .collect(),
                );
            }
            // the row lost the focus when it was removed
            row.grab_focus();
        }
        #[cfg(test)]
        pub(super) fn add_synthetic_row(&self, id: &str, name: &str) {
            let row = TvLiveChannelSelectorRow::new(&ChannelId::from(id.to_owned()), name);
            row.connect_move_requested(glib::clone!(
                #[weak(rename_to = slf)]
                self,
                move |row, offset| slf.move_row(row, offset)
            ));
            row.set_visible(true);
            self.visible_channels.append(&row);
        }

        fn remove_row(&self, row: &TvLiveChannelSelectorRow) {
            if let Some(pos) = self.visible_channels.find(row) {
                self.visible_channels.remove(pos)
            }
            if let Some(pos) = self.hidden_channels.find(row) {
                self.hidden_channels.remove(pos)
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TvLiveChannelSelector {
        const NAME: &'static str = "TvLiveChannelSelector";
        type Type = super::TvLiveChannelSelector;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();

            klass.install_action(
                "live-channel-selector.sort-alphabetically",
                None,
                |slf, _, _| {
                    let slf = slf.imp();
                    slf.visible_channels
                        .typed_sort::<TvLiveChannelSelectorRow>(|a, b| {
                            a.channel_name()
                                .to_lowercase()
                                .cmp(&b.channel_name().to_lowercase())
                        })
                },
            );
            klass.install_action(
                "live-channel-selector.sort-default-order",
                None,
                |slf, _, _| {
                    let slf = slf.imp();
                    let channel_rows = slf.channel_rows.borrow();

                    slf.visible_channels
                        .typed_sort::<TvLiveChannelSelectorRow>(|a, b| {
                            channel_rows
                                .get_index_of(&a.channel_id())
                                .cmp(&channel_rows.get_index_of(&b.channel_id()))
                        })
                },
            );
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TvLiveChannelSelector {
        fn constructed(&self) {
            self.parent_constructed();
            install_channel_menu_remote_handler(&self.sort_button);

            self.visible_channel_rows
                .bind_model(Some(&self.visible_channels), |row| {
                    row.clone().downcast().unwrap()
                });
            self.hidden_channel_rows
                .bind_model(Some(&self.hidden_channels), |row| {
                    row.clone().downcast().unwrap()
                });

            self.loading.set(true);
            #[cfg(test)]
            let suppress_load = SUPPRESS_LIVE_LOAD.with(Cell::get);
            #[cfg(not(test))]
            let suppress_load = false;
            if !suppress_load {
                spawn(glib::clone!(
                    #[weak(rename_to = slf)]
                    self,
                    async move { slf.load().await }
                ));
            }

            let settings = self.settings.clone();
            let owner = self.obj().downgrade();
            self.visible_channels
                .connect_items_changed(move |visible_channels, _, _, _| {
                    let rows = visible_channels
                        .iter::<TvLiveChannelSelectorRow>()
                        .map(|res| res.unwrap())
                        .collect::<Vec<_>>();

                    // the first row can't move up and the last one down
                    for (position, row) in rows.iter().enumerate() {
                        row.action_set_enabled("row.move-up", position > 0);
                        row.action_set_enabled("row.move-down", position + 1 < rows.len());
                    }

                    // Rebuilding the initial channel rows must not save a
                    // transient order or cause a Live programme reload.
                    if owner.upgrade().is_some_and(|slf| !slf.imp().loading.get()) {
                        settings
                            .set_live_channels(rows.iter().map(|row| row.channel_id()).collect());
                    }
                });
        }
    }
    impl WidgetImpl for TvLiveChannelSelector {}
    impl NavigationPageImpl for TvLiveChannelSelector {}
}

glib::wrapper! {
    pub struct TvLiveChannelSelector(ObjectSubclass<imp::TvLiveChannelSelector>)
        @extends gtk::Widget, adw::NavigationPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TvLiveChannelSelector {
    pub fn new() -> Self {
        glib::Object::new()
    }
}

#[cfg(test)]
mod native_channel_menu_tests {
    use super::*;
    use std::{cell::Cell, rc::Rc, time::Duration};

    thread_local! {
        static INITIAL_X11_FOCUS_SET: Cell<bool> = const { Cell::new(false) };
    }

    fn spin() {
        for _ in 0..20 {
            while glib::MainContext::default().pending() {
                glib::MainContext::default().iteration(false);
            }
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    // X11/XTest injects real GDK key events into a mapped native surface.
    fn native_key(key: &str) {
        // Only establish X11 focus for the first key. Re-focusing the last
        // root child after a popdown targets a now-unmapped popup (BadMatch)
        // and defeats GTK's real focus restoration.
        let initial_focus = INITIAL_X11_FOCUS_SET.with(|flag| !flag.replace(true));
        let script = r#"
import ctypes as c
import sys
x, t = c.CDLL("libX11.so.6"), c.CDLL("libXtst.so.6")
x.XOpenDisplay.argtypes = [c.c_char_p]
x.XOpenDisplay.restype = c.c_void_p
d = x.XOpenDisplay(None)
assert d, "X11 display missing"
x.XDefaultRootWindow.argtypes = [c.c_void_p]
x.XDefaultRootWindow.restype = c.c_ulong
x.XQueryTree.argtypes = [c.c_void_p, c.c_ulong, c.POINTER(c.c_ulong),
    c.POINTER(c.c_ulong), c.POINTER(c.POINTER(c.c_ulong)), c.POINTER(c.c_uint)]
x.XQueryTree.restype = c.c_int
root = x.XDefaultRootWindow(d)
ret_root, parent, count = c.c_ulong(), c.c_ulong(), c.c_uint()
children = c.POINTER(c.c_ulong)()
assert x.XQueryTree(d, root, c.byref(ret_root), c.byref(parent),
    c.byref(children), c.byref(count))
assert count.value, "no mapped GTK window"
x.XSetInputFocus.argtypes = [c.c_void_p, c.c_ulong, c.c_int, c.c_ulong]
if sys.argv[2] == "initial":
    x.XSetInputFocus(d, children[count.value - 1], 1, 0)
x.XFree.argtypes = [c.c_void_p]
x.XFree(c.cast(children, c.c_void_p))
x.XStringToKeysym.argtypes = [c.c_char_p]
x.XStringToKeysym.restype = c.c_ulong
x.XKeysymToKeycode.argtypes = [c.c_void_p, c.c_ulong]
x.XKeysymToKeycode.restype = c.c_uint
sym = x.XStringToKeysym(sys.argv[1].encode())
assert sym, "unknown keysym " + sys.argv[1]
code = x.XKeysymToKeycode(d, sym)
assert code, "unmapped keysym " + sys.argv[1]
t.XTestFakeKeyEvent.argtypes = [c.c_void_p, c.c_uint, c.c_int, c.c_ulong]
assert t.XTestFakeKeyEvent(d, code, 1, 0)
assert t.XTestFakeKeyEvent(d, code, 0, 0)
x.XFlush.argtypes = [c.c_void_p]
x.XFlush(d)
x.XCloseDisplay.argtypes = [c.c_void_p]
x.XCloseDisplay(d)
"#;
        let focus_mode = if initial_focus { "initial" } else { "keep" };
        let output = std::process::Command::new("python3")
            .args(["-c", script, key, focus_mode])
            .output()
            .expect("XTest Python driver");
        assert!(
            output.status.success(),
            "native {key} injection: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        spin();
    }

    fn find_menu(widget: &gtk::Widget, tooltip: &str) -> Option<gtk::MenuButton> {
        if let Some(button) = widget.downcast_ref::<gtk::MenuButton>() {
            if button.tooltip_text().as_deref() == Some(tooltip) {
                return Some(button.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            if let Some(button) = find_menu(&next, tooltip) {
                return Some(button);
            }
            child = next.next_sibling();
        }
        None
    }

    fn synthetic_page() -> TvLiveChannelSelector {
        SUPPRESS_LIVE_LOAD.with(|skip| skip.set(true));
        let page = TvLiveChannelSelector::new();
        SUPPRESS_LIVE_LOAD.with(|skip| skip.set(false));
        for (id, name) in [
            ("fixture-channel-a", "Alpha"),
            ("fixture-channel-b", "Bravo"),
            ("fixture-channel-c", "Charlie"),
        ] {
            page.imp().add_synthetic_row(id, name);
        }
        page.imp().loading.set(false);
        page
    }

    fn menu_owns_focus(window: &crate::window::TvWindow, button: &gtk::MenuButton) -> bool {
        gtk::prelude::GtkWindowExt::focus(window).is_some_and(|focused| {
            focused == button.clone().upcast::<gtk::Widget>() || focused.is_ancestor(button)
        })
    }

    fn row(page: &TvLiveChannelSelector, index: i32) -> TvLiveChannelSelectorRow {
        page.imp()
            .visible_channel_rows
            .row_at_index(index)
            .expect("mapped live row")
            .downcast()
            .expect("TvLiveChannelSelectorRow")
    }

    #[test]
    #[ignore = "requires mapped GTK/Xvfb, XTest and memory GSettings"]
    fn osk_mapped_gtk_channel_move_menu_native_remote() {
        adw::init().expect("GTK display");
        let resources = gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
            .expect("bundled resources");
        gio::resources_register(&resources);
        let app = TvApplication::new();
        // GtkApplicationWindow registration is only valid after startup.
        // Register synchronously before constructing the mapped test window.
        app.register(None::<&gio::Cancellable>)
            .expect("test application startup");
        let win = crate::window::TvWindow::new(&app);
        win.set_default_size(1152, 760);
        win.present();
        let preferences = crate::preferences::TvPreferencesDialog::new();
        let page = synthetic_page();
        preferences.push_subpage(&page);
        preferences.present(Some(&win));
        spin();
        assert!(page.is_mapped(), "real Preferences subpage");

        let settings = TvSettings::get();
        let saves = Rc::new(Cell::new(0));
        let observed = saves.clone();
        settings.connect_live_channels_changed(move |_| {
            observed.set(observed.get() + 1);
        });

        let second = row(&page, 1);
        let button = find_menu(second.upcast_ref(), "Move").expect("Move menu");
        let popup = button.popover().expect("real PopoverMenu");
        assert!(button.grab_focus());
        native_key("Return");
        assert!(popup.is_mapped(), "Return opens menu");
        native_key("XF86Back");
        assert!(!popup.is_mapped(), "Back closes Move popup");
        assert!(page.is_mapped(), "Back retains Preferences subpage");
        assert!(
            menu_owns_focus(&win, &button),
            "Back restores focus into Move button; actual focus: {:?}",
            gtk::prelude::GtkWindowExt::focus(&win)
                .as_ref()
                .map(|widget| widget.type_().name().to_string())
        );
        assert_eq!(saves.get(), 0, "dismissal must not reorder");

        // No synthetic grab_focus here: native Return must work after Back.
        native_key("Return");
        assert!(popup.is_mapped(), "reopen after Back");
        native_key("Escape");
        assert!(!popup.is_mapped(), "native Escape dismisses menu");
        assert_eq!(saves.get(), 0);

        assert!(button.grab_focus());
        native_key("Return");
        assert!(popup.is_mapped());
        native_key("Down");
        native_key("Return");
        assert!(!popup.is_mapped(), "Move action closes popup");
        assert_eq!(saves.get(), 1, "one GSettings commit on move");
        let names: Vec<_> = page
            .imp()
            .visible_channels
            .iter::<TvLiveChannelSelectorRow>()
            .map(|r| r.unwrap().channel_name())
            .collect();
        assert_ne!(
            names,
            ["Alpha", "Bravo", "Charlie"],
            "native move changes order"
        );

        let first = row(&page, 0);
        let last = row(&page, 2);
        // GtkWidget.activate_action returns success when an action exists,
        // even if its disabled state prevents a callback. Test the observable
        // contract instead: no boundary action may reorder or persist.
        let before_boundary = names.clone();
        let before_saves = saves.get();
        let _ = first.activate_action("row.move-up", None);
        let _ = last.activate_action("row.move-down", None);
        let after_boundary: Vec<_> = page
            .imp()
            .visible_channels
            .iter::<TvLiveChannelSelectorRow>()
            .map(|r| r.unwrap().channel_name())
            .collect();
        assert_eq!(after_boundary, before_boundary, "boundary cannot reorder");
        assert_eq!(saves.get(), before_saves, "boundary cannot persist");
        let last_button = find_menu(last.upcast_ref(), "Move").unwrap();
        assert!(last_button.grab_focus());
        native_key("Return");
        assert!(last_button.popover().unwrap().is_mapped());
        native_key("XF86Back");
        assert!(!last_button.popover().unwrap().is_mapped());

        native_key("XF86Back");
        assert!(!page.is_mapped(), "second Back pops subpage normally");
        assert!(preferences.is_mapped(), "Preferences retained");
        preferences.push_subpage(&page);
        spin();
        let first_button = find_menu(row(&page, 0).upcast_ref(), "Move").unwrap();
        assert!(first_button.grab_focus());
        native_key("Return");
        assert!(first_button.popover().unwrap().is_mapped());
        native_key("XF86HomePage");
        assert!(!first_button.popover().unwrap().is_mapped());
        assert!(!preferences.is_mapped(), "Home closes Preferences");
        win.destroy();
    }

    // Regression probe: exercise the first-row Move popup while it remains
    // mapped, using the same real XTest path as the Back/Home tests.
    // Keep this test separate so it can establish a before-fix failure.
    #[test]
    #[ignore = "requires mapped GTK/Xvfb, XTest and memory GSettings"]
    fn osk_mapped_gtk_first_row_move_popup_native_focus_containment() {
        adw::init().expect("GTK display");
        let resources = gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
            .expect("bundled resources");
        gio::resources_register(&resources);
        let app = TvApplication::new();
        app.register(None::<&gio::Cancellable>)
            .expect("test application startup");
        let win = crate::window::TvWindow::new(&app);
        win.set_default_size(1152, 760);
        win.present();
        let preferences = crate::preferences::TvPreferencesDialog::new();
        let page = synthetic_page();
        preferences.push_subpage(&page);
        preferences.present(Some(&win));
        spin();
        assert!(page.is_mapped(), "real Preferences subpage");

        let first = row(&page, 0);
        let button = find_menu(first.upcast_ref(), "Move").expect("first Move menu");
        let popup = button.popover().expect("real GtkPopoverMenu");
        assert!(button.grab_focus());
        native_key("Return");
        assert!(popup.is_mapped(), "first-row Move popup must be mapped");

        // Inspect the actual mapped GTK menu widget hierarchy, including
        // effective sensitivity; the action installer alone proves neither
        // boundary-menu item is interactive.
        fn inspect(widget: &gtk::Widget, depth: usize) {
            if depth > 7 {
                return;
            }
            eprintln!(
                "F02 menu widget depth={} type={} mapped={} sensitive={} focusable={} focused={}",
                depth,
                widget.type_().name(),
                widget.is_mapped(),
                widget.is_sensitive(),
                widget.is_focusable(),
                widget.has_focus()
            );
            let mut child = widget.first_child();
            while let Some(next) = child {
                inspect(&next, depth + 1);
                child = next.next_sibling();
            }
        }
        inspect(popup.upcast_ref(), 0);

        // No test-induced focus reset between these native directional keys.
        // Capture the first escape and its exact preceding key.
        for (step, key) in [
            "Up", "Down", "Up", "Left", "Right", "Down", "Down",
            "Up", "Left", "Right", "Up", "Down",
        ]
        .iter()
        .enumerate()
        {
            native_key(key);
            let focused = gtk::prelude::GtkWindowExt::focus(&win);
            let in_popup = focused.as_ref().is_some_and(|widget| {
                widget == popup.upcast_ref::<gtk::Widget>() || widget.is_ancestor(&popup)
            });
            let in_visible_list = focused.as_ref().is_some_and(|widget| {
                widget.is_ancestor(&*page.imp().visible_channel_rows)
            });
            let in_hidden_list = focused.as_ref().is_some_and(|widget| {
                widget.is_ancestor(&*page.imp().hidden_channel_rows)
            });
            let focus_ancestry = focused
                .as_ref()
                .map(|widget| {
                    let mut names = Vec::new();
                    let mut node = Some(widget.clone());
                    while let Some(current) = node {
                        names.push(current.type_().name().to_string());
                        node = current.parent();
                    }
                    names.join(" <- ")
                })
                .unwrap_or_else(|| "NONE".to_string());
            eprintln!(
                "F02 step={} key={} mapped={} visible={} popup_focus={} visible_list_focus={} hidden_list_focus={} GtkWindow::focus={}",
                step + 1,
                key,
                popup.is_mapped(),
                popup.is_visible(),
                in_popup,
                in_visible_list,
                in_hidden_list,
                focus_ancestry
            );
            assert!(
                popup.is_mapped() && in_popup && !in_visible_list && !in_hidden_list,
                "F02 first-row focus escaped at native step {} key {}: popup mapped={} focus={}",
                step + 1,
                key,
                popup.is_mapped(),
                focus_ancestry
            );
        }
        button.popdown();
        win.destroy();
    }

    #[test]
    #[ignore = "requires mapped GTK/Xvfb, XTest and memory GSettings"]
    fn osk_mapped_gtk_channel_sort_menu_native_remote() {
        adw::init().expect("GTK display");
        let resources = gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
            .expect("bundled resources");
        gio::resources_register(&resources);
        let app = TvApplication::new();
        // GtkApplicationWindow registration is only valid after startup.
        // Register synchronously before constructing the mapped test window.
        app.register(None::<&gio::Cancellable>)
            .expect("test application startup");
        let win = crate::window::TvWindow::new(&app);
        win.set_default_size(1152, 760);
        win.present();
        let preferences = crate::preferences::TvPreferencesDialog::new();
        let page = synthetic_page();
        preferences.push_subpage(&page);
        preferences.present(Some(&win));
        spin();
        let sort = find_menu(page.upcast_ref(), "Sort").expect("header Sort button");
        let popup = sort.popover().expect("real Sort PopoverMenu");
        assert!(sort.grab_focus());
        native_key("Return");
        assert!(popup.is_mapped(), "Sort popup mapped");
        native_key("XF86Back");
        assert!(!popup.is_mapped(), "Sort should dismiss on native Back");
        assert!(page.is_mapped(), "Sort Back stays on Preferences subpage");
        assert!(
            menu_owns_focus(&win, &sort),
            "Sort Back restores button focus; actual focus: {:?}",
            gtk::prelude::GtkWindowExt::focus(&win)
                .as_ref()
                .map(|widget| widget.type_().name().to_string())
        );
        // Reopen using native Return without a test-induced focus reset.
        native_key("Return");
        assert!(popup.is_mapped());
        native_key("XF86HomePage");
        assert!(!popup.is_mapped(), "Sort Home closes popup");
        assert!(!preferences.is_mapped(), "Sort Home closes Preferences");
        win.destroy();
    }
}
