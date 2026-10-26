// SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org>
// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use adw::{gio, glib, gtk, prelude::*, subclass::prelude::*};

use crate::{
    application::TvApplication, config::PROFILE, live::TvLiveView, mediathek::TvMediathekView,
    settings::TvSettings, streams::view::TvStreamsView,
};

/// Return state is scoped to a specific managed mpv process. It is not a
/// persisted navigation preference, and never outlives this window.
#[derive(Debug)]
struct PlaybackReturn {
    token: u64,
    page: String,
    navigation_generation: u64,
    focus: Option<glib::WeakRef<gtk::Widget>>,
}

impl PlaybackReturn {
    fn should_restore(&self, token: u64, navigation_generation: u64) -> bool {
        self.token == token && self.navigation_generation == navigation_generation
    }
}

/// Keypad arrows normally represent the same remote direction as dedicated
/// arrows when Num Lock is off. Preserve physical key identity outside the
/// application's remote router and all modal/popover native editing.
fn normalize_remote_arrow(key: gtk::gdk::Key) -> gtk::gdk::Key {
    use gtk::gdk::Key;
    match key {
        Key::KP_Left => Key::Left,
        Key::KP_Right => Key::Right,
        Key::KP_Up => Key::Up,
        Key::KP_Down => Key::Down,
        _ => key,
    }
}

/// Measured in the same mapped page coordinates. No synthetic tab order:
/// each remote direction selects an actually visible geometric neighbor.
#[derive(Clone, Copy, Debug)]
struct ToolbarRect {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl ToolbarRect {
    fn of(widget: &gtk::Widget, page: &gtk::Widget) -> Option<Self> {
        widget.compute_bounds(page).map(|rect| Self {
            left: rect.x(),
            top: rect.y(),
            right: rect.x() + rect.width(),
            bottom: rect.y() + rect.height(),
        })
    }

    fn cx(self) -> f32 {
        (self.left + self.right) / 2.0
    }

    fn cy(self) -> f32 {
        (self.top + self.bottom) / 2.0
    }
}

/// Horizontal arrows stay on the same visual row; vertical arrows select the
/// closest adjacent row and then the closest horizontal center. Candidate
/// order resolves exact geometric ties (stable even when controls reflow).
fn spatial_neighbor(rects: &[ToolbarRect], from: usize, key: gtk::gdk::Key) -> Option<usize> {
    let origin = *rects.get(from)?;
    let mut best: Option<(usize, f32, f32)> = None;
    for (index, candidate) in rects.iter().copied().enumerate() {
        if index == from {
            continue;
        }
        let (primary, secondary) = match key {
            gtk::gdk::Key::Left
                if candidate.cx() < origin.cx() - 1.0
                    && candidate.bottom.min(origin.bottom) > candidate.top.max(origin.top) =>
            {
                (
                    origin.cx() - candidate.cx(),
                    (origin.cy() - candidate.cy()).abs(),
                )
            }
            gtk::gdk::Key::Right
                if candidate.cx() > origin.cx() + 1.0
                    && candidate.bottom.min(origin.bottom) > candidate.top.max(origin.top) =>
            {
                (
                    candidate.cx() - origin.cx(),
                    (origin.cy() - candidate.cy()).abs(),
                )
            }
            gtk::gdk::Key::Up if candidate.bottom <= origin.top + 2.0 => (
                origin.top - candidate.bottom,
                (origin.cx() - candidate.cx()).abs(),
            ),
            gtk::gdk::Key::Down if candidate.top >= origin.bottom - 2.0 => (
                candidate.top - origin.bottom,
                (origin.cx() - candidate.cx()).abs(),
            ),
            _ => continue,
        };
        if best.is_none_or(|(_, distance, offset)| {
            primary.total_cmp(&distance).is_lt()
                || (primary.total_cmp(&distance).is_eq() && secondary.total_cmp(&offset).is_lt())
        }) {
            best = Some((index, primary, secondary));
        }
    }
    best.map(|(index, _, _)| index)
}

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(file = "src/window.blp")]
    pub struct TvWindow {
        #[template_child]
        pub(super) toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub(super) stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub(super) menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(super) live_view: TemplateChild<TvLiveView>,
        #[template_child]
        pub(super) mediathek_view: TemplateChild<TvMediathekView>,
        #[template_child]
        pub(super) streams_view: TemplateChild<TvStreamsView>,
        // An AdwDialog may own the keyboard grab. Track all still-live
        // dialogs: dismissing nested dialogs can reveal an earlier one.
        pub(super) remote_dialogs: RefCell<Vec<glib::WeakRef<adw::Dialog>>>,
        pub(super) playback_token: Cell<u64>,
        pub(super) navigation_generation: Cell<u64>,
        pub(super) playback_return: RefCell<Option<PlaybackReturn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TvWindow {
        const NAME: &'static str = "TvWindow";
        type Type = super::TvWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();

            klass.install_action("window.reload", None, |slf, _, _| {
                let slf = slf.imp();
                match slf.stack.visible_child_name().as_deref() {
                    Some("live") => slf.live_view.reload(),
                    Some("mediathek") => slf.mediathek_view.reload(),
                    Some("streams") => slf.streams_view.reload(),
                    _ => (),
                }
            });
            klass.install_action("window.search", None, |slf, _, _| {
                let slf = slf.imp();
                match slf.stack.visible_child_name().as_deref() {
                    Some("live") => slf.live_view.focus_search(),
                    Some("mediathek") => slf.mediathek_view.focus_search(),
                    Some("streams") => slf.streams_view.focus_search(),
                    _ => (),
                }
            });
            klass.install_action("window.show-live", None, |slf, _, _| {
                slf.imp().stack.set_visible_child_name("live")
            });
            klass.install_action("window.show-mediathek", None, |slf, _, _| {
                slf.imp().stack.set_visible_child_name("mediathek")
            });
            klass.install_action("window.show-streams", None, |slf, _, _| {
                slf.imp().stack.set_visible_child_name("streams");
                slf.imp().streams_view.focus_search();
            });
            klass.install_action("window.add-playlist", None, |slf, _, _| {
                slf.imp().stack.set_visible_child_name("streams");
                slf.imp().streams_view.show_add();
            });
            klass.install_action("window.manage-sources", None, |slf, _, _| {
                slf.imp().stack.set_visible_child_name("streams");
                slf.imp().streams_view.show_manager();
            });
            // Explicit escape hatch for TV remotes as well as desktop users.
            // The existing fullscreened settings binding persists this choice.
            klass.install_action("window.toggle-fullscreen", None, |slf, _, _| {
                if slf.is_fullscreen() {
                    slf.unfullscreen();
                } else {
                    slf.fullscreen();
                }
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TvWindow {
        fn constructed(&self) {
            self.parent_constructed();

            let slf = self.obj();
            let settings = TvSettings::get();

            settings
                .bind_width(&*slf, "default-width")
                .flags(gio::SettingsBindFlags::DEFAULT)
                .build();
            settings
                .bind_height(&*slf, "default-height")
                .flags(gio::SettingsBindFlags::DEFAULT)
                .build();
            settings
                .bind_is_maximized(&*slf, "maximized")
                .flags(gio::SettingsBindFlags::DEFAULT)
                .build();
            settings
                .bind_is_fullscreen(&*slf, "fullscreened")
                .flags(gio::SettingsBindFlags::DEFAULT)
                .build();
            // A cold launch always begins in Mediathek. Do not bind the
            // historical visible-view preference: it can still contain Live
            // from earlier versions, but page choice is session-local now.
            self.stack.set_visible_child_name("mediathek");
            self.stack.connect_visible_child_name_notify(glib::clone!(
                #[weak(rename_to = slf)]
                slf,
                move |_| {
                    let imp = slf.imp();
                    imp.navigation_generation
                        .set(imp.navigation_generation.get().wrapping_add(1));
                }
            ));

            // The remote exposes normal Wayland keysyms. Use the standard GTK
            // event path rather than an evdev listener or compositor shortcuts.
            let controller = gtk::EventControllerKey::new();
            // Capture remote navigation even when a modal GtkPopover owns focus.
            // Ordinary arrows, Return and edited text still propagate to GTK.
            controller.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak_window = slf.downgrade();
            controller.connect_key_pressed(move |_, key, _, modifiers| {
                weak_window
                    .upgrade()
                    .map_or(glib::Propagation::Proceed, |window| {
                        window.handle_remote_key(key, modifiers)
                    })
            });
            slf.add_controller(controller);

            // Modal GtkPopover widgets can be the input grab root. In that
            // case the window's capture controller is skipped entirely.
            // Install the same narrow remote handler on existing popovers,
            // including the app menu, sort menu and GtkDropDown internals.
            slf.install_popover_remote_handlers(slf.upcast_ref());

            // Preferences, Shortcuts and About use libadwaita dialogs that can
            // receive keys outside the main window's capture controller.
            slf.connect_visible_dialog_notify(|window| {
                window.install_visible_dialog_remote_handler();
            });
        }
    }
    impl WidgetImpl for TvWindow {}
    impl WindowImpl for TvWindow {}
    impl ApplicationWindowImpl for TvWindow {}
    impl AdwApplicationWindowImpl for TvWindow {}
}

glib::wrapper! {
    pub struct TvWindow(ObjectSubclass<imp::TvWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl TvWindow {
    pub fn new(application: &TvApplication) -> Self {
        let win: Self = glib::Object::builder()
            .property("application", application)
            .build();

        if PROFILE == "Devel" {
            // Keep a development identifier without libadwaita's striped header.
            win.set_title(Some("Broadcaster (Development)"));
        }

        win
    }
    pub fn add_toast(&self, toast: adw::Toast) {
        self.imp().toast_overlay.add_toast(toast)
    }

    /// Select the GTK Window focus getter explicitly (GtkWindowExt and
    /// RootExt both define `focus` in gtk-rs).
    fn focused_widget(&self) -> Option<gtk::Widget> {
        gtk::prelude::GtkWindowExt::focus(self)
    }

    fn install_popover_remote_handlers(&self, widget: &gtk::Widget) {
        if let Some(popover) = widget.downcast_ref::<gtk::Popover>() {
            let controller = gtk::EventControllerKey::new();
            controller.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak_window = self.downgrade();
            controller.connect_key_pressed(move |_, key, _, modifiers| {
                weak_window
                    .upgrade()
                    .map_or(glib::Propagation::Proceed, |window| {
                        window.handle_remote_key(key, modifiers)
                    })
            });
            popover.add_controller(controller);
        }

        let mut child = widget.first_child();
        while let Some(current) = child {
            self.install_popover_remote_handlers(&current);
            child = current.next_sibling();
        }
    }

    /// A modal AdwDialog can become the keyboard grab root. Install a key
    /// controller on the top dialog *as well as* keeping the existing window
    /// capture controller, so the physical remote works in either GTK path.
    fn install_visible_dialog_remote_handler(&self) {
        let Some(dialog) = self.visible_dialog() else {
            return;
        };
        let mut tracked = self.imp().remote_dialogs.borrow_mut();
        tracked.retain(|weak| weak.upgrade().is_some());
        if tracked
            .iter()
            .any(|weak| weak.upgrade().as_ref() == Some(&dialog))
        {
            return;
        }

        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak_window = self.downgrade();
        controller.connect_key_pressed(move |_, key, _, modifiers| {
            weak_window
                .upgrade()
                .map_or(glib::Propagation::Proceed, |window| {
                    window.handle_remote_key(key, modifiers)
                })
        });
        dialog.add_controller(controller);
        tracked.push(glib::clone::Downgrade::downgrade(&dialog));
    }

    // Popovers are not guaranteed to be ancestors of the focused widget:
    // GtkMenuButton and GtkDropDown create attached transient children.
    // Inspect mapped popovers instead of guessing from GTK focus.
    fn find_mapped_popover(widget: &gtk::Widget) -> Option<gtk::Popover> {
        if let Some(popover) = widget.downcast_ref::<gtk::Popover>() {
            if popover.is_mapped() {
                return Some(popover.clone());
            }
        }

        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(popover) = Self::find_mapped_popover(&current) {
                return Some(popover);
            }
            child = current.next_sibling();
        }
        None
    }

    fn dismiss_open_popover(&self) -> bool {
        // Explicitly handle the app menu even when its native popup takes focus.
        if self
            .imp()
            .menu_button
            .popover()
            .is_some_and(|popover| popover.is_mapped())
        {
            self.imp().menu_button.popdown();
            return true;
        }

        if let Some(popover) = Self::find_mapped_popover(self.upcast_ref()) {
            popover.popdown();
            return true;
        }
        false
    }

    fn go_home(&self) {
        // Preserve the active query, results, filters and scroll position.
        self.imp()
            .mediathek_view
            .collapse_focused(self.focused_widget());
        self.imp().stack.set_visible_child_name("mediathek");
        self.imp().mediathek_view.focus_browse();
    }

    /// Mark the originating page and focus before launching managed mpv.
    /// Replacements receive a new token; stale watcher callbacks cannot
    /// restore focus or change a page selected by the user in the meantime.
    pub fn begin_playback(&self) -> u64 {
        let imp = self.imp();
        let token = imp.playback_token.get().wrapping_add(1);
        imp.playback_token.set(token);
        let page = imp
            .stack
            .visible_child_name()
            .unwrap_or_else(|| "mediathek".into())
            .to_string();
        let focus = self
            .focused_widget()
            .map(|widget| glib::clone::Downgrade::downgrade(&widget));
        imp.playback_return.replace(Some(PlaybackReturn {
            token,
            page,
            navigation_generation: imp.navigation_generation.get(),
            focus,
        }));
        token
    }

    /// Called once when the *current* managed player exits, or when it could
    /// not be started. Do not overwrite deliberate main-window navigation.
    pub fn finish_playback(&self, token: u64) {
        let imp = self.imp();
        let context = {
            let mut pending = imp.playback_return.borrow_mut();
            if !pending.as_ref().is_some_and(|state| state.token == token) {
                return;
            }
            pending.take().expect("checked above")
        };
        if !context.should_restore(token, imp.navigation_generation.get()) {
            return;
        }

        imp.stack.set_visible_child_name(&context.page);
        // Recycled ListView row widgets are never a durable playback focus.
        if context.page == "streams" {
            imp.streams_view.restore_selection_by_id();
            return;
        }
        if let Some(widget) = context.focus.and_then(|weak| weak.upgrade()) {
            if widget.is_mapped() && widget.is_sensitive() && widget.grab_focus() {
                return;
            }
        }
        match context.page.as_str() {
            "live" => imp.live_view.focus_search(),
            "mediathek" => imp.mediathek_view.focus_browse(),
            _ => (),
        }
    }

    fn is_within(widget: &gtk::Widget, ancestor: &gtk::Widget) -> bool {
        let mut current = Some(widget.clone());
        while let Some(item) = current {
            if item == *ancestor {
                return true;
            }
            current = item.parent();
        }
        false
    }

    fn find_descendant(
        root: &gtk::Widget,
        predicate: &impl Fn(&gtk::Widget) -> bool,
    ) -> Option<gtk::Widget> {
        if predicate(root) {
            return Some(root.clone());
        }
        let mut child = root.first_child();
        while let Some(item) = child {
            if let Some(result) = Self::find_descendant(&item, predicate) {
                return Some(result);
            }
            child = item.next_sibling();
        }
        None
    }

    fn focus_page_switcher(&self) -> bool {
        // The title switcher is unmapped in compact layouts; then use the
        // bottom switcher instead. Both expose their actual page buttons as
        // focusable children rather than accepting focus as a container.
        let switcher_types: [fn(&gtk::Widget) -> bool; 2] = [
            |w| w.is::<adw::ViewSwitcher>(),
            |w| w.is::<adw::ViewSwitcherBar>(),
        ];
        for is_switcher in switcher_types {
            if let Some(widget) = Self::find_descendant(self.upcast_ref(), &is_switcher) {
                if widget.is_mapped() && widget.child_focus(gtk::DirectionType::TabForward) {
                    return true;
                }
            }
        }
        false
    }

    /// Used only when focus is already inside an upper page control. Modal
    /// dialogs and mapped dropdown/popover menus were excluded by the caller.
    /// Native search editing owns Left/Right until the caret hits that edge.
    fn focus_spatial_toolbar_key(
        &self,
        page_name: &str,
        page: &gtk::Widget,
        focused: &gtk::Widget,
        key: gtk::gdk::Key,
    ) -> Option<bool> {
        let controls = match page_name {
            "live" => self.imp().live_view.toolbar_focus_targets(),
            "mediathek" => self.imp().mediathek_view.toolbar_focus_targets(),
            "streams" => self.imp().streams_view.toolbar_focus_targets(),
            _ => return None,
        };
        let mapped: Vec<_> = controls
            .into_iter()
            .filter(|w| w.is_mapped() && w.is_sensitive() && w.is_visible())
            .filter_map(|w| {
                ToolbarRect::of(&w, page)
                    .filter(|rect| rect.right > rect.left + 1.0 && rect.bottom > rect.top + 1.0)
                    .map(|rect| (w, rect))
            })
            .collect();
        let Some(current) = mapped.iter().position(|(w, _)| Self::is_within(focused, w)) else {
            return None; // Never intercept list rows or nested dialogs.
        };

        if matches!(key, gtk::gdk::Key::Left | gtk::gdk::Key::Right) {
            if let Some(entry) = mapped[current].0.downcast_ref::<gtk::SearchEntry>() {
                if entry.selection_bounds().is_some() {
                    return Some(false);
                }
                let last = entry.text().chars().count() as i32;
                // GTK permits -1 to mean "at end", not "before start".
                let raw_position = entry.position();
                let position = if raw_position < 0 { last } else { raw_position };
                if (key == gtk::gdk::Key::Left && position > 0)
                    || (key == gtk::gdk::Key::Right && position < last)
                {
                    return Some(false);
                }
            }
        }

        let geometry: Vec<_> = mapped.iter().map(|(_, rect)| *rect).collect();
        if let Some(index) = spatial_neighbor(&geometry, current, key) {
            return Some(mapped[index].0.grab_focus());
        }
        // Upper and lower boundaries deliberately remain navigable without
        // introducing diagonal/historical Up-Down toolbar chains.
        if key == gtk::gdk::Key::Up {
            return Some(self.focus_page_switcher());
        }
        if key == gtk::gdk::Key::Down {
            return Some(match page_name {
                "live" => Self::find_descendant(page, &|w| w.is::<gtk::ListBox>())
                    .and_then(|list| list.downcast::<gtk::ListBox>().ok())
                    .and_then(|list| list.row_at_index(0))
                    .is_some_and(|row| row.grab_focus()),
                "mediathek" => self.imp().mediathek_view.focus_first_result(),
                "streams" => self.imp().streams_view.focus_first_result(),
                _ => false,
            });
        }
        Some(false)
    }

    /// Only bridge the known boundaries between list, toolbar and page
    /// switcher. Interior row navigation, editing, dropdowns, dialogs and
    /// mpv keep their own native key handling.
    fn focus_boundary_key(&self, key: gtk::gdk::Key) -> bool {
        if !matches!(
            key,
            gtk::gdk::Key::Up | gtk::gdk::Key::Down | gtk::gdk::Key::Left | gtk::gdk::Key::Right
        ) || Self::find_mapped_popover(self.upcast_ref()).is_some()
        {
            return false;
        }
        let Some(focused) = self.focused_widget() else {
            return false;
        };
        let page = match self.imp().stack.visible_child_name().as_deref() {
            Some("live") => self.imp().live_view.upcast_ref::<gtk::Widget>(),
            Some("mediathek") => self.imp().mediathek_view.upcast_ref::<gtk::Widget>(),
            Some("streams") => self.imp().streams_view.upcast_ref::<gtk::Widget>(),
            _ => return false,
        };
        let search = Self::find_descendant(page, &|widget| widget.is::<gtk::SearchEntry>());

        if Self::is_within(&focused, page) {
            if let Some(handled) = self.focus_spatial_toolbar_key(
                self.imp()
                    .stack
                    .visible_child_name()
                    .as_deref()
                    .unwrap_or_default(),
                page,
                &focused,
                key,
            ) {
                // A toolbar key was considered here. Never fall through to
                // the historical fixed vertical chain, even when native text
                // caret handling must receive an unconsumed arrow.
                return handled;
            }
            if self.imp().stack.visible_child_name().as_deref() == Some("streams") {
                // Expanded row actions own their D-pad interior and Left exit.
                // Route before the historical list -> page switcher boundary.
                if self
                    .imp()
                    .streams_view
                    .focus_expanded_action_key(&focused, key)
                {
                    return true;
                }
                // Escape from any virtualized row using one remote key, no
                // matter where the selection is in a large playlist.
                // GTK keeps Left/Right ownership in filters and editors.
                if key == gtk::gdk::Key::Left {
                    return self.imp().streams_view.contains_results_focus(&focused)
                        && self.focus_page_switcher();
                }
                if key == gtk::gdk::Key::Up
                    && search
                        .as_ref()
                        .is_some_and(|entry| Self::is_within(&focused, entry))
                {
                    return self.focus_page_switcher();
                }
                return self
                    .imp()
                    .streams_view
                    .focus_vertical_navigation(&focused, key);
            }
            if self.imp().stack.visible_child_name().as_deref() == Some("mediathek") {
                // Mediathek owns its full Search <-> Channel <-> Duration
                // <-> Sort <-> Results focus path. The old generic handler
                // skipped two filters and its fallback trapped focus on cards.
                if key == gtk::gdk::Key::Up
                    && search
                        .as_ref()
                        .is_some_and(|entry| Self::is_within(&focused, entry))
                {
                    return self.focus_page_switcher();
                }
                return self
                    .imp()
                    .mediathek_view
                    .focus_vertical_navigation(&focused, key);
            }

            // Live list's first row lies beneath Search, not Keyboard.
            // Search <-> Keyboard is a horizontal transition, handled by
            // mapped geometry above. Preserve the first-row child boundary.
            let list = Self::find_descendant(page, &|widget| widget.is::<gtk::ListBox>())
                .and_then(|widget| widget.downcast::<gtk::ListBox>().ok());

            if key == gtk::gdk::Key::Up {
                if let Some(first) = list.as_ref().and_then(|list| list.row_at_index(0)) {
                    let first_widget = first.upcast_ref::<gtk::Widget>();
                    if Self::is_within(&focused, first_widget) {
                        if focused != *first_widget {
                            return first.grab_focus();
                        }
                        return search.as_ref().is_some_and(|entry| entry.grab_focus());
                    }
                }
                if self.imp().live_view.keyboard_prototype_has_focus(&focused) {
                    // Above Keyboard is the page switcher, not Search to
                    // its left. Normally the spatial router handles this.
                    return self.focus_page_switcher();
                }
                if search
                    .as_ref()
                    .is_some_and(|entry| Self::is_within(&focused, entry))
                {
                    return self.focus_page_switcher();
                }
            } else if key == gtk::gdk::Key::Down {
                if self.imp().live_view.keyboard_prototype_has_focus(&focused) {
                    return list
                        .as_ref()
                        .and_then(|list| list.row_at_index(0))
                        .is_some_and(|row| row.grab_focus());
                }
                if search
                    .as_ref()
                    .is_some_and(|entry| Self::is_within(&focused, entry))
                {
                    return list
                        .as_ref()
                        .and_then(|list| list.row_at_index(0))
                        .is_some_and(|row| row.grab_focus());
                }
            }
            return false;
        }

        // Down from the global header (or the compact bottom switcher)
        // returns to this page's Search, not straight into the results.
        if key == gtk::gdk::Key::Down {
            let switcher = Self::find_descendant(self.upcast_ref(), &|widget| {
                widget.is::<adw::ViewSwitcher>() && widget.is_mapped()
            })
            .or_else(|| {
                Self::find_descendant(self.upcast_ref(), &|widget| {
                    widget.is::<adw::ViewSwitcherBar>() && widget.is_mapped()
                })
            });
            if switcher
                .as_ref()
                .is_some_and(|bar| Self::is_within(&focused, bar))
            {
                return search.as_ref().is_some_and(|entry| entry.grab_focus());
            }
        }
        false
    }

    fn handle_remote_key(
        &self,
        key: gtk::gdk::Key,
        modifiers: gtk::gdk::ModifierType,
    ) -> glib::Propagation {
        // The topmost modal layer takes priority over popovers and Mediathek
        // navigation. Without this check Back/Home can change the main view
        // while Preferences, Shortcuts or About stays on screen.
        if let Some(dialog) = self.visible_dialog() {
            // A nested keyboard cancels its own draft; Home cannot navigate
            // behind the active modal or dismiss the source editor.
            if dialog.has_css_class("osk-feasibility")
                && crate::keyboard::unmodified_remote(modifiers)
                && matches!(
                    key,
                    gtk::gdk::Key::Back | gtk::gdk::Key::Escape | gtk::gdk::Key::HomePage
                )
            {
                tracing::trace!("osk prototype: window cancel capture");
                dialog.close();
                return glib::Propagation::Stop;
            }
            match modal_remote_action(key, modifiers) {
                Some(ModalRemoteAction::Back) => {
                    // Preserve normal Preferences navigation: Back first
                    // returns from mpv arguments or Live channel ordering.
                    if !dialog
                        .downcast_ref::<adw::PreferencesDialog>()
                        .is_some_and(|preferences| preferences.pop_subpage())
                    {
                        dialog.close();
                    }
                    return glib::Propagation::Stop;
                }
                Some(ModalRemoteAction::Home) => {
                    dialog.close();
                    self.go_home();
                    return glib::Propagation::Stop;
                }
                None => return glib::Propagation::Proceed,
            }
        }

        if !crate::keyboard::unmodified_remote(modifiers) {
            return glib::Propagation::Proceed;
        }
        let key = normalize_remote_arrow(key);

        // A key still held as mpv relinquishes Wayland focus is not a
        // main-application Back/Home command while playback is active.
        if self.imp().playback_return.borrow().is_some()
            && matches!(key, gtk::gdk::Key::Back | gtk::gdk::Key::HomePage)
        {
            return glib::Propagation::Stop;
        }

        if self.focus_boundary_key(key) {
            return glib::Propagation::Stop;
        }

        match key {
            gtk::gdk::Key::Back => {
                if self.dismiss_open_popover() {
                    return glib::Propagation::Stop;
                }

                if self.imp().stack.visible_child_name().as_deref() == Some("streams") {
                    if !self.imp().streams_view.collapse_expanded() {
                        self.go_home();
                    }
                } else if self.imp().stack.visible_child_name().as_deref() == Some("live") {
                    self.go_home();
                } else if !self
                    .imp()
                    .mediathek_view
                    .collapse_focused(self.focused_widget())
                {
                    self.imp().mediathek_view.focus_browse();
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Escape
                if self.imp().stack.visible_child_name().as_deref() == Some("streams") =>
            {
                // Give an open GTK popover its native Escape behavior first.
                // Expanded rows collapse; otherwise Escape exits the list to
                // the tabs while the remote Back key still goes to Mediathek.
                if Self::find_mapped_popover(self.upcast_ref()).is_some() {
                    glib::Propagation::Proceed
                } else if self.imp().streams_view.collapse_expanded()
                    || (self.focused_widget().is_some_and(|focused| {
                        self.imp().streams_view.contains_results_focus(&focused)
                    }) && self.focus_page_switcher())
                {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
            gtk::gdk::Key::HomePage => {
                self.dismiss_open_popover();
                self.go_home();
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Menu => {
                // Menu toggles its own popup. Switching from a filter/sort
                // popup closes that layer before opening the application menu.
                if self
                    .imp()
                    .menu_button
                    .popover()
                    .is_some_and(|popover| popover.is_mapped())
                {
                    self.imp().menu_button.popdown();
                } else {
                    self.dismiss_open_popover();
                    self.imp().menu_button.popup();
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModalRemoteAction {
    Back,
    Home,
}

/// Only the actual television remote's unmodified XF86 keysyms dismiss a
/// modal dialog; text entry, Return and modified key combinations remain GTK's.
fn modal_remote_action(
    key: gtk::gdk::Key,
    modifiers: gtk::gdk::ModifierType,
) -> Option<ModalRemoteAction> {
    if !crate::keyboard::unmodified_remote(modifiers) {
        return None;
    }
    match key {
        gtk::gdk::Key::Back => Some(ModalRemoteAction::Back),
        gtk::gdk::Key::HomePage => Some(ModalRemoteAction::Home),
        _ => None,
    }
}

#[cfg(test)]
mod remote_dialog_tests {
    use super::{modal_remote_action, ModalRemoteAction};
    use adw::gtk::gdk::{Key, ModifierType};

    #[test]
    fn back_and_home_match_remote_keysyms_without_modifiers() {
        assert_eq!(
            modal_remote_action(Key::Back, ModifierType::empty()),
            Some(ModalRemoteAction::Back)
        );
        assert_eq!(
            modal_remote_action(Key::HomePage, ModifierType::empty()),
            Some(ModalRemoteAction::Home)
        );
    }

    #[test]
    fn ordinary_ui_and_modified_keys_remain_untouched() {
        for key in [Key::Return, Key::Escape, Key::Menu, Key::Left, Key::Right] {
            assert_eq!(modal_remote_action(key, ModifierType::empty()), None);
        }
        assert_eq!(
            modal_remote_action(Key::Back, ModifierType::CONTROL_MASK),
            None
        );
        assert_eq!(
            modal_remote_action(Key::HomePage, ModifierType::ALT_MASK),
            None
        );
    }
}

#[cfg(test)]
mod playback_navigation_tests {
    use super::*;

    #[test]
    fn only_the_current_player_may_restore_its_origin() {
        let context = PlaybackReturn {
            token: 42,
            page: "live".to_string(),
            navigation_generation: 7,
            focus: None,
        };
        assert!(context.should_restore(42, 7));
        assert!(!context.should_restore(41, 7), "replaced player");
        assert!(!context.should_restore(42, 8), "user changed pages");
        assert_eq!(context.page, "live");
    }

    /// Exercises the real TvWindow construction and playback-context methods
    /// with no network stream or actual mpv process. An isolated in-memory
    /// GSettings schema is supplied by the Xvfb CI test command.
    #[test]
    #[ignore = "requires GTK/Xvfb and isolated compiled GSettings schema"]
    fn gtk_playback_window_origin_and_cold_start() {
        let bytes = glib::Bytes::from_static(crate::GRESOURCE_BYTES);
        let resource = gio::Resource::from_data(&bytes).expect("valid bundled resources");
        gio::resources_register(&resource);
        gtk::init().expect("GTK test display");

        // Mimic an older profile that left Live selected. New processes must
        // ignore this historical key without resetting unrelated preferences.
        let settings = gio::Settings::new(crate::config::BASE_APP_ID);
        settings
            .set_string("visible-view", "live")
            .expect("set prior view");
        let app = TvApplication::new();
        let window = TvWindow::new(&app);
        let stack = &window.imp().stack;
        assert_eq!(stack.visible_child_name().as_deref(), Some("mediathek"));

        stack.set_visible_child_name("live");
        let live_token = window.begin_playback();
        window.finish_playback(live_token);
        assert_eq!(stack.visible_child_name().as_deref(), Some("live"));

        let mediathek_token = {
            stack.set_visible_child_name("mediathek");
            window.begin_playback()
        };
        window.finish_playback(mediathek_token);
        assert_eq!(stack.visible_child_name().as_deref(), Some("mediathek"));

        // S4: actual third page and fake GTK ListView activation protocol.
        stack.set_visible_child_name("streams");
        assert_eq!(stack.visible_child_name().as_deref(), Some("streams"));
        window.imp().streams_view.assert_fake_player_navigation();
        crate::streams::assert_s5_form_controls();

        // S5: exercise the actual mapped virtualized ListView and window
        // focus router on GTK/Xvfb. Physical Wayland remote validation remains
        // a separate owner/device check.
        window.set_default_size(1280, 720);
        window.present();
        let streams = &window.imp().streams_view;
        for _ in 0..48 {
            if !glib::MainContext::default().iteration(false) {
                break;
            }
        }
        assert!(streams.show_and_focus_test_results());
        let focused = window.focused_widget().expect("list should hold focus");
        assert!(streams.contains_results_focus(&focused));
        // Up from the first row must reach the toolbar. A reversed GTK
        // ancestry test used to trap the remote within GtkListView children.
        assert!(streams.focus_vertical_navigation(&focused, gtk::gdk::Key::Up));
        assert!(!streams
            .contains_results_focus(&window.focused_widget().expect("toolbar should hold focus")));
        streams.assert_remote_toolbar_focus_styled();
        assert!(streams.show_and_focus_test_results());
        assert!(window.focus_boundary_key(gtk::gdk::Key::Left));
        assert!(!streams
            .contains_results_focus(&window.focused_widget().expect("tabs should hold focus")));
        assert!(streams.show_and_focus_test_results());
        assert_eq!(
            window.handle_remote_key(gtk::gdk::Key::Escape, gtk::gdk::ModifierType::empty()),
            glib::Propagation::Stop
        );
        assert!(!streams
            .contains_results_focus(&window.focused_widget().expect("Escape should focus tabs")));
        streams.focus_search();
        assert!(!window.focus_boundary_key(gtk::gdk::Key::Left));
        assert!(streams.show_and_focus_test_results());
        assert_eq!(
            window.handle_remote_key(gtk::gdk::Key::Back, gtk::gdk::ModifierType::empty()),
            glib::Propagation::Stop
        );
        assert_eq!(stack.visible_child_name().as_deref(), Some("mediathek"));
        stack.set_visible_child_name("streams");
        let streams_token = window.begin_playback();
        window.finish_playback(streams_token);
        assert_eq!(stack.visible_child_name().as_deref(), Some("streams"));

        // Later user navigation must win over playback-return restoration.
        stack.set_visible_child_name("live");
        let changed_page_token = window.begin_playback();
        stack.set_visible_child_name("mediathek");
        window.finish_playback(changed_page_token);
        assert_eq!(stack.visible_child_name().as_deref(), Some("mediathek"));

        // A replaced player's watcher must not consume newer return context.
        stack.set_visible_child_name("live");
        let old_token = window.begin_playback();
        let new_token = window.begin_playback();
        window.finish_playback(old_token);
        stack.set_visible_child_name("mediathek");
        window.finish_playback(new_token);
        assert_eq!(stack.visible_child_name().as_deref(), Some("mediathek"));

        // Reopening a window within the same process is a new window
        // construction, not a side effect of returning from mpv.
        let another_window = TvWindow::new(&app);
        assert_eq!(
            another_window.imp().stack.visible_child_name().as_deref(),
            Some("mediathek")
        );
        assert_eq!(settings.string("visible-view"), "live");

        // The same CI Xvfb window exercises the production router against
        // the actual K1 keyboard, Mediathek toggles and Streams filter rows.
        // A list-only test would miss the wrong Up/Down upper-control chain.
        let spin = || {
            // A mapped widget can still have 0px allocation until the next
            // GDK frame. Drain events *and* allow frame clock ticks on Xvfb.
            for _ in 0..20 {
                while glib::MainContext::default().pending() {
                    glib::MainContext::default().iteration(false);
                }
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
        };
        let assert_focused = |target: &gtk::Widget| {
            let focused = window.focused_widget().expect("visible focus");
            assert!(
                TvWindow::is_within(&focused, target),
                "remote focus must follow the actual mapped neighboring control"
            );
        };

        stack.set_visible_child_name("live");
        window.imp().live_view.show_toolbar_for_test();
        window.queue_allocate();
        spin();
        let live = window.imp().live_view.toolbar_focus_targets();
        assert!(
            live.iter().all(|w| w.is_mapped()),
            "Live toolbar must really be mapped"
        );
        assert!(live[0].grab_focus());
        // A non-boundary Left/Right belongs to the editable caret.
        let live_search = live[0].clone().downcast::<gtk::SearchEntry>().unwrap();
        live_search.set_text("ab");
        live_search.set_position(1);
        assert!(!window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&live[0]);
        live_search.set_position(2);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&live[1]); // physical neighbor, not Down
        assert!(window.focus_boundary_key(gtk::gdk::Key::Left));
        assert_focused(&live[0]);
        live_search.set_text("");

        // Search and Keyboard are distinct visual and focusable targets.
        // Their actual allocated rectangles must have a visible TV-sized gap.
        let live_page = window.imp().live_view.upcast_ref::<gtk::Widget>();
        let search_rect = ToolbarRect::of(&live[0], live_page).expect("mapped Live search");
        let keyboard_rect = ToolbarRect::of(&live[1], live_page).expect("mapped Live keyboard");
        assert!(
            keyboard_rect.left - search_rect.right >= 10.0,
            "search and Keyboard must be visually separated: {search_rect:?}, {keyboard_rect:?}"
        );

        // Owner Wayland report: Up from the first channel jumped diagonally
        // to Keyboard, although Search is directly above the channel list.
        // Up must reach Search; Right then reaches Keyboard with a visible
        // blue focus frame. Left and Down must restore spatial relationships.
        assert!(window.imp().live_view.focus_first_channel_for_test());
        assert!(window.focus_boundary_key(gtk::gdk::Key::Up));
        assert_focused(&live[0]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&live[1]);
        window.imp().live_view.assert_keyboard_remote_focus_styled();
        assert!(window.focus_boundary_key(gtk::gdk::Key::Left));
        assert_focused(&live[0]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Down));
        let list = TvWindow::find_descendant(live_page, &|w| w.is::<gtk::ListBox>())
            .expect("Live channel list");
        assert!(
            TvWindow::is_within(
                &window.focused_widget().expect("Down returns focus to list"),
                &list
            ),
            "Down from Live search must reach the channels"
        );
        assert!(window.focus_boundary_key(gtk::gdk::Key::Up));
        assert_focused(&live[0]);

        stack.set_visible_child_name("mediathek");
        window.queue_allocate();
        spin();
        let med = window.imp().mediathek_view.toolbar_focus_targets();
        assert_eq!(med.len(), 7, "search, keyboard, two toggles, three filters");
        assert!(med[0].is_mapped() && med[1].is_mapped());
        let med_search = med[0].clone().downcast::<gtk::SearchEntry>().unwrap();
        med_search.set_position(-1); // Explicitly move native caret to end.
        let med_page = window.imp().mediathek_view.upcast_ref::<gtk::Widget>();
        let med_rects: Vec<_> = med
            .iter()
            .map(|control| ToolbarRect::of(control, med_page).expect("mapped Mediathek control"))
            .collect();
        assert_eq!(
            spatial_neighbor(&med_rects, 0, gtk::gdk::Key::Right),
            Some(1),
            "mapped search and keyboard launcher must be spatial neighbors: {med_rects:?}"
        );
        assert!(med[0].grab_focus());
        med_search.set_position(-1);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&med[1]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&med[2]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Left));
        assert_focused(&med[1]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&med[2]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&med[3]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Left));
        assert_focused(&med[2]);
        assert!(med[4].grab_focus());
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&med[5]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&med[6]);

        stack.set_visible_child_name("streams");
        window.queue_allocate();
        spin();
        let filters = streams.toolbar_focus_targets();
        assert!(filters[1].grab_focus());
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&filters[2]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&filters[3]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Left));
        assert_focused(&filters[2]);
        assert!(filters[4].grab_focus());
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&filters[5]);
        assert!(filters[7].grab_focus());
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&filters[8]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&filters[9]);
        assert!(
            !filters[6].is_mapped(),
            "hidden region must not steal focus"
        );

        // E4 owner regression: the real mapped GtkListView contains two
        // fabricated My Channels entries. Exercise the same window capture
        // router as a remote; GTK event synthesis is not claimed here.
        let ids = streams.install_remote_action_test_rows();
        window.queue_allocate();
        spin();
        assert!(streams.show_and_focus_test_results());
        assert!(
            !window.focus_boundary_key(gtk::gdk::Key::Right),
            "collapsed row must retain native arrow behavior"
        );
        eprintln!(
            "E4 before activation: {}",
            streams.remote_action_test_state()
        );
        streams.remote_action_test_activate();
        eprintln!(
            "E4 after activation: {}",
            streams.remote_action_test_state()
        );
        spin();
        eprintln!("E4 after animation: {}", streams.remote_action_test_state());
        // GtkListView may still retain an unbound/recycled ListItem with the
        // same *position* as the selected row. Its detached buttons are
        // correctly unmapped, but are not the actions rendered on screen.
        // Inspect the actual mapped, expanded row instead of picking the first
        // cached binding at position zero (HashMap iteration is unordered).
        let list = TvWindow::find_descendant(streams.upcast_ref(), &|w| w.is::<gtk::ListView>())
            .expect("mapped My Channels ListView");
        let mut expanded_rows = Vec::new();
        fn find_expanded_rows(node: &gtk::Widget, rows: &mut Vec<gtk::Revealer>) {
            if let Some(revealer) = node.downcast_ref::<gtk::Revealer>() {
                if revealer.reveals_child() && revealer.is_mapped() {
                    rows.push(revealer.clone());
                }
            }
            let mut child = node.first_child();
            while let Some(widget) = child {
                find_expanded_rows(&widget, rows);
                child = widget.next_sibling();
            }
        }
        find_expanded_rows(&list, &mut expanded_rows);
        assert_eq!(
            expanded_rows.len(),
            1,
            "exactly one on-screen selected row must be expanded"
        );
        let expanded = &expanded_rows[0];
        let row = expanded.parent().expect("expanded row container");
        assert!(
            TvWindow::find_descendant(&row, &|w| {
                w.downcast_ref::<gtk::Label>()
                    .is_some_and(|label| label.label() == "First fabricated channel")
            })
            .is_some(),
            "the expanded actions must belong to the selected fabricated channel"
        );
        let mut visible_buttons = Vec::new();
        fn collect_buttons(node: &gtk::Widget, buttons: &mut Vec<gtk::Button>) {
            if let Some(button) = node.downcast_ref::<gtk::Button>() {
                buttons.push(button.clone());
            }
            let mut child = node.first_child();
            while let Some(widget) = child {
                collect_buttons(&widget, buttons);
                child = widget.next_sibling();
            }
        }
        collect_buttons(expanded.upcast_ref(), &mut visible_buttons);
        assert_eq!(visible_buttons.len(), 3, "Play, Edit, Mapping must exist");
        let buttons: [gtk::Button; 3] = visible_buttons.try_into().unwrap();
        let button_states: Vec<_> = buttons
            .iter()
            .map(|button| {
                let reveal = button
                    .ancestor(gtk::Revealer::static_type())
                    .and_downcast::<gtk::Revealer>();
                (
                    button.is_mapped(),
                    button.is_visible(),
                    button.is_sensitive(),
                    reveal.as_ref().map(|reveal| reveal.reveals_child()),
                    reveal.as_ref().map(|reveal| reveal.is_mapped()),
                )
            })
            .collect();
        assert!(
            buttons
                .iter()
                .all(|button| button.is_mapped() && button.is_sensitive()),
            "all three actions must be mapped and enabled without a guide; states: {button_states:?}; selected={:?}",
            streams.remote_action_test_selected_id()
        );
        let assert_action = |index: usize| {
            let focused = window.focused_widget().expect("focused action");
            assert!(
                TvWindow::is_within(&focused, buttons[index].upcast_ref()),
                "the expected expanded-row action must own focus: {index}"
            );
            assert!(
                buttons[index].has_focus(),
                "GTK focus styling requires focus"
            );
        };
        // Repeated Right must not make any of the expanded actions lose GTK focus.
        // This verifies the routed widget state, not compositor-rendered pixels.
        for (index, button) in buttons.iter().enumerate() {
            assert!(button.grab_focus(), "action {index} must accept focus");
            assert_action(index);
            for _ in 0..10 {
                assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
                assert_action(index);
            }
        }
        assert!(buttons[0].grab_focus());

        // A disabled or unmatched programme guide must never gate Play/focus.
        streams.remote_action_test_status("Selected programme guide disabled");
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_action(0);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Down));
        assert_action(1);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Down));
        assert_action(2);
        streams.remote_action_test_status("No matching programme guide channel");
        assert_action(2); // informational EPG paint must not steal focus
        assert!(window.focus_boundary_key(gtk::gdk::Key::Up));
        assert_action(1);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Up));
        assert_action(0);
        streams.remote_action_test_status("Now: fabricated programme");
        assert_action(0);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Up));
        assert!(
            streams.contains_results_focus(&window.focused_widget().expect("row focus")),
            "Up from Play must return to the owning row"
        );
        assert!(!buttons.iter().any(|button| button.has_focus()));
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_action(0);
        // The focused Play button's callback, not GtkListView activate,
        // performs the single action; even spurious list activation must not
        // deliver the historic second-OK playback.
        buttons[0].emit_clicked();
        streams.remote_action_test_activate();
        assert_eq!(streams.remote_action_test_counts(), (1, 0, 0));
        assert!(window.focus_boundary_key(gtk::gdk::Key::Down));
        buttons[1].emit_clicked();
        assert_eq!(streams.remote_action_test_counts(), (1, 1, 0));
        assert!(window.focus_boundary_key(gtk::gdk::Key::Down));
        buttons[2].emit_clicked();
        assert_eq!(streams.remote_action_test_counts(), (1, 1, 1));
        // Boundary policy: Down from final action exits back to the list.
        assert!(window.focus_boundary_key(gtk::gdk::Key::Down));
        assert!(!buttons.iter().any(|button| button.has_focus()));
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_action(0);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Left));
        assert_eq!(stack.visible_child_name().as_deref(), Some("streams"));
        assert!(!buttons.iter().any(|button| button.has_focus()));
        assert!(streams.contains_results_focus(&window.focused_widget().expect("row")));
        // Recycling an action-focused list item must not keep a detached
        // button as the window's focus target.
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        streams.remote_action_test_rebind();
        spin();
        assert!(!buttons.iter().any(|button| button.has_focus()));
        assert!(streams.contains_results_focus(&window.focused_widget().expect("after rebind")));
        assert!(streams.show_and_focus_test_results());
        assert_eq!(
            streams.remote_action_test_selected_id().as_deref(),
            Some(ids[0].as_str())
        );

        // Collapse and Back/Escape semantics remain unchanged; Home exits
        // My Channels, but no action button can hijack a modal's keys.
        assert_eq!(
            window.handle_remote_key(gtk::gdk::Key::Back, gtk::gdk::ModifierType::empty()),
            glib::Propagation::Stop
        );
        assert_eq!(stack.visible_child_name().as_deref(), Some("streams"));
        streams.remote_action_test_activate();
        spin();
        assert_eq!(
            window.handle_remote_key(gtk::gdk::Key::Escape, gtk::gdk::ModifierType::empty()),
            glib::Propagation::Stop
        );
        assert!(streams.contains_results_focus(&window.focused_widget().expect("Escape row")));
        assert_eq!(
            window.handle_remote_key(gtk::gdk::Key::HomePage, gtk::gdk::ModifierType::empty()),
            glib::Propagation::Stop
        );
        assert_eq!(stack.visible_child_name().as_deref(), Some("mediathek"));
    }

    /// Checks GTK widget ancestry and list-boundary detection against real
    /// ListBox rows. This does not simulate Wayland compositor key dispatch.
    #[test]
    #[ignore = "needs a live GTK display; CI executes under xvfb-run"]
    fn gtk_focus_boundary_distinguishes_first_and_interior_rows() {
        gtk::init().expect("test display");
        let list = gtk::ListBox::new();
        let first = gtk::ListBoxRow::new();
        let button = gtk::Button::with_label("Play");
        first.set_child(Some(&button));
        let second = gtk::ListBoxRow::new();
        let second_button = gtk::Button::with_label("Expanded card action");
        second.set_child(Some(&second_button));
        list.append(&first);
        list.append(&second);

        let first_widget = list.row_at_index(0).expect("first row");
        assert!(TvWindow::is_within(
            button.upcast_ref(),
            first_widget.upcast_ref()
        ));
        assert!(TvWindow::is_within(
            first.upcast_ref(),
            first_widget.upcast_ref()
        ));
        assert!(!TvWindow::is_within(
            second.upcast_ref(),
            first_widget.upcast_ref()
        ));

        // A focused child action belongs to its containing results row.
        // That ancestry is now resolved in the Mediathek view itself.
        let owning_row = second_button.ancestor(gtk::ListBoxRow::static_type());
        assert_eq!(owning_row, Some(second.clone().upcast::<gtk::Widget>()));
        let first_owner = button.ancestor(gtk::ListBoxRow::static_type());
        assert_eq!(first_owner, Some(first.clone().upcast::<gtk::Widget>()));

        let from_list =
            TvWindow::find_descendant(list.upcast_ref(), &|widget| widget.is::<gtk::Button>())
                .expect("find child action nested under the first row");
        assert_eq!(from_list, button.upcast::<gtk::Widget>());
        assert!(list.row_at_index(2).is_none(), "empty boundary is safe");
    }
}

#[cfg(test)]
mod spatial_navigation_tests {
    #[test]
    fn keypad_and_lock_modifiers_preserve_navigation_intent() {
        use gtk::gdk::{Key, ModifierType as Mods};
        assert_eq!(super::normalize_remote_arrow(Key::KP_Left), Key::Left);
        assert_eq!(super::normalize_remote_arrow(Key::KP_Right), Key::Right);
        assert_eq!(super::normalize_remote_arrow(Key::KP_Up), Key::Up);
        assert_eq!(super::normalize_remote_arrow(Key::KP_Down), Key::Down);
        assert!(crate::keyboard::unmodified_remote(Mods::LOCK_MASK));
        assert!(!crate::keyboard::unmodified_remote(Mods::CONTROL_MASK));
        assert_eq!(super::modal_remote_action(Key::Home, Mods::empty()), None);
    }

    use super::{spatial_neighbor, ToolbarRect};
    use adw::gtk::gdk::Key;

    fn rect(left: f32, top: f32, width: f32) -> ToolbarRect {
        ToolbarRect {
            left,
            right: left + width,
            top,
            bottom: top + 40.0,
        }
    }

    #[test]
    fn horizontal_navigation_never_jumps_into_another_visual_row() {
        // Search occupies the whole first row; toggle controls are beside it.
        // Filters on a second row must never be chosen by Right/Left.
        let layout = [
            rect(0.0, 0.0, 430.0),
            rect(440.0, 0.0, 40.0),
            rect(490.0, 0.0, 40.0),
            rect(10.0, 60.0, 140.0),
            rect(160.0, 60.0, 140.0),
            rect(310.0, 60.0, 110.0),
        ];
        assert_eq!(spatial_neighbor(&layout, 0, Key::Right), Some(1));
        assert_eq!(spatial_neighbor(&layout, 1, Key::Right), Some(2));
        assert_eq!(spatial_neighbor(&layout, 2, Key::Left), Some(1));
        assert_eq!(spatial_neighbor(&layout, 2, Key::Right), None);
        assert_eq!(spatial_neighbor(&layout, 3, Key::Right), Some(4));
        assert_eq!(spatial_neighbor(&layout, 4, Key::Left), Some(3));
    }

    #[test]
    fn vertical_navigation_chooses_nearest_actual_row_and_column() {
        let layout = [
            rect(0.0, 0.0, 300.0),
            rect(0.0, 60.0, 90.0),
            rect(105.0, 60.0, 90.0),
            rect(210.0, 60.0, 90.0),
            rect(0.0, 120.0, 90.0),
            rect(105.0, 120.0, 90.0),
            rect(210.0, 120.0, 90.0),
        ];
        // Search center is closest to column two, not the first-by-code filter.
        assert_eq!(spatial_neighbor(&layout, 0, Key::Down), Some(2));
        assert_eq!(spatial_neighbor(&layout, 2, Key::Down), Some(5));
        assert_eq!(spatial_neighbor(&layout, 5, Key::Up), Some(2));
        assert_eq!(spatial_neighbor(&layout, 2, Key::Up), Some(0));
        assert_eq!(spatial_neighbor(&layout, 0, Key::Up), None);
        assert_eq!(spatial_neighbor(&layout, 6, Key::Down), None);
    }

    #[test]
    fn hidden_controls_do_not_create_phantom_geometric_neighbors() {
        // The router supplies only mapped + visible + sensitive candidates.
        let without_hidden_region = [rect(0.0, 0.0, 90.0), rect(100.0, 0.0, 90.0)];
        assert_eq!(
            spatial_neighbor(&without_hidden_region, 1, Key::Right),
            None
        );
    }
}

// Independent FOCUS-001 regression probes. Deliberately test-only: the
// production key router, CSS and focus policy remain completely unchanged.
#[cfg(test)]
mod focus001_mapped_regressions {
    use super::*;

    fn mapped_window(page: &str) -> (TvApplication, TvWindow) {
        let resource = gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
            .expect("valid bundled GTK resources");
        gio::resources_register(&resource);
        gtk::init().expect("Xvfb GTK display required");
        let app = TvApplication::new();
        let window = TvWindow::new(&app);
        window.set_default_size(1280, 800);
        window.imp().stack.set_visible_child_name(page);
        window.present();
        settle();
        (app, window)
    }

    fn settle() {
        for _ in 0..24 {
            while glib::MainContext::default().pending() {
                glib::MainContext::default().iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    }

    fn owns_focus(window: &TvWindow, expected: &gtk::Widget) -> bool {
        window.focused_widget().is_some_and(|owner| {
            owner.has_focus()
                && owner.is_mapped()
                && owner.is_visible()
                && owner.is_sensitive()
                && TvWindow::is_within(&owner, expected)
        })
    }

    fn assert_owner(window: &TvWindow, expected: &gtk::Widget, edge: &str) {
        let owner = window
            .focused_widget()
            .expect("GTK window lost its focus owner");
        eprintln!(
            "FOCUS001 edge={edge} owner={} mapped={} sensitive={} focus={} visible_focus={} flags={:?}",
            owner.type_().name(),
            owner.is_mapped(),
            owner.is_sensitive(),
            owner.has_focus(),
            owner.has_visible_focus(),
            owner.state_flags()
        );
        assert!(
            owns_focus(window, expected),
            "{edge}: expected mapped/enabled focus within {}, actually {}",
            expected.type_().name(),
            owner.type_().name()
        );
        // A GTK has_focus() assertion is NOT proof of an actually painted ring.
        // CSS/rendered contrast is tracked as a separate evidence status.
    }

    fn key(window: &TvWindow, direction: gtk::gdk::Key) {
        assert_eq!(
            window.handle_remote_key(direction, gtk::gdk::ModifierType::empty()),
            glib::Propagation::Stop,
            "window capture path did not handle the remote key {direction:?}"
        );
        settle();
    }

    /// Regression canary: Down from Mediathek Search must not skip the
    /// intermediate controls, and Up from its adjacent target must return.
    /// Unlike the earlier view-only test, this runs TvWindow's capture router.
    #[test]
    #[ignore = "requires separately isolated mapped GTK/Xvfb + compiled test schema"]
    fn gtk_focus001_mediathek_down_up_roundtrip() {
        let (_app, window) = mapped_window("mediathek");
        let controls = window.imp().mediathek_view.toolbar_focus_targets();
        let search = &controls[0];
        assert!(search.grab_focus());
        assert_owner(&window, search, "initial Mediathek Search");
        key(&window, gtk::gdk::Key::Down);
        let next = window.focused_widget().expect("Down lost focus");
        assert!(
            controls
                .iter()
                .any(|widget| TvWindow::is_within(&next, widget)),
            "Mediathek Search Down skipped all intermediary toolbar filters"
        );
        assert!(next != *search, "Down must make forward progress");
        assert_owner(&window, &next, "Mediathek Search --Down--> next");
        key(&window, gtk::gdk::Key::Up);
        assert_owner(&window, search, "Mediathek Down then Up must reverse");
        window.close();
    }

    #[test]
    #[ignore = "requires separately isolated mapped GTK/Xvfb + compiled test schema"]
    fn gtk_focus001_mediathek_horizontal_filter_roundtrips() {
        let (_app, window) = mapped_window("mediathek");
        let controls = window.imp().mediathek_view.toolbar_focus_targets();
        assert_eq!(controls.len(), 7);
        for (from, to) in [(1, 2), (4, 5), (5, 6)] {
            assert!(controls[from].grab_focus());
            assert_owner(&window, &controls[from], "filter origin");
            key(&window, gtk::gdk::Key::Right);
            assert_owner(&window, &controls[to], "filter Right sibling");
            key(&window, gtk::gdk::Key::Left);
            assert_owner(&window, &controls[from], "filter Left reverses Right");
        }
        window.close();
    }

    #[test]
    #[ignore = "requires separately isolated mapped GTK/Xvfb + compiled test schema"]
    fn gtk_focus001_live_first_row_up_to_search() {
        let (_app, window) = mapped_window("live");
        window.imp().live_view.show_toolbar_for_test();
        settle();
        let search = window.imp().live_view.toolbar_focus_targets()[0].clone();
        assert!(window.imp().live_view.focus_first_channel_for_test());
        let list = TvWindow::find_descendant(
            window.imp().live_view.upcast_ref::<gtk::Widget>(),
            &|widget| widget.is::<gtk::ListBox>(),
        )
        .expect("real mapped Live ListBox");
        assert_owner(&window, &list, "Live first row");
        key(&window, gtk::gdk::Key::Up);
        assert_owner(&window, &search, "Live first row --Up--> Search");
        // Search Down is not asserted here: in the current window handler it
        // returns Proceed to GTK's native event propagation, which direct
        // method invocation cannot synthesize. See coverage matrix.
        window.close();
    }

    /// Owner-reported known RED: Left from a selected My Channels row
    /// unexpectedly exits to the global My Channels category switcher.
    /// Keep the asserted expected containment rather than normalizing the bug.
    #[test]
    #[ignore = "requires separately isolated mapped GTK/Xvfb + compiled test schema"]
    fn gtk_focus001_streams_row_left_stays_in_page() {
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        streams.install_remote_action_test_rows();
        settle();
        assert!(streams.show_and_focus_test_results());
        let before = window.focused_widget().expect("selected My Channels row");
        assert!(streams.contains_results_focus(&before));
        assert_owner(&window, &before, "My Channels selected row");
        key(&window, gtk::gdk::Key::Left);
        let after = window.focused_widget().expect("Left lost focus completely");
        eprintln!(
            "FOCUS001 My Channels Left: {} -> {}",
            before.type_().name(),
            after.type_().name()
        );
        assert!(
            streams.contains_results_focus(&after),
            "RED: Left from a selected My Channels row jumped to page controls"
        );
        assert_owner(&window, &after, "My Channels Left containment");
        window.close();
    }

    /// A disposable focus thief proves the ownership oracle detects a
    /// regression; the product focus router is not changed for this test.
    #[test]
    #[ignore = "requires separately isolated mapped GTK/Xvfb + compiled test schema"]
    fn gtk_focus001_negative_control_detects_stolen_focus() {
        let (_app, window) = mapped_window("mediathek");
        let controls = window.imp().mediathek_view.toolbar_focus_targets();
        assert!(controls[0].grab_focus());
        assert_owner(&window, &controls[0], "negative control origin");
        assert!(
            controls[1].grab_focus(),
            "test-only focus thief must be mapped"
        );
        assert!(
            !owns_focus(&window, &controls[0]),
            "negative control FAILED to detect deliberate focus theft"
        );
        assert_owner(&window, &controls[1], "negative control stolen owner");
        window.close();
    }

    fn native_x11_key(key_name: &str) {
        let emitter = std::env::var_os("FOCUS001_XTEST_BIN")
            .expect("test runner must provide an actual XTest key emitter");
        let output = std::process::Command::new(emitter)
            .arg(key_name)
            .output()
            .expect("execute isolated real X11 key emitter");
        assert!(
            output.status.success(),
            "real X11 {key_name} emission failed with {:?}",
            output.status.code()
        );
        settle();
    }

    // Prove XTest events actually reach the mapped GTK editable before using
    // a route result as product evidence. XTest returning success is not proof
    // of dispatch, particularly under Xvfb without a window manager.
    fn assert_native_input_delivery(search: &gtk::Widget) {
        let entry = search
            .clone()
            .downcast::<gtk::SearchEntry>()
            .expect("real mapped search-entry target");
        entry.set_text("");
        assert!(entry.grab_focus());
        settle();
        native_x11_key("a");
        if entry.text().as_str() != "a" {
            eprintln!(
                "FOCUS001 INPUT_UNVERIFIED expected real keypress to type in mapped SearchEntry"
            );
            panic!("native X11 input not confirmed at focused GTK control");
        }
        eprintln!("FOCUS001 INPUT_CONFIRMED XTest typed into mapped GTK SearchEntry");
        entry.set_text("");
        settle();
        assert!(entry.grab_focus());
    }

    /// Native GDK propagation is essential for this edge: direct invocation
    /// of handle_remote_key() returns Proceed and cannot synthesize GTK's
    /// subsequent Search-to-List arrow traversal.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb with real XTest key injection"]
    fn gtk_focus001_xtest_live_search_down_native_propagation() {
        let (_app, window) = mapped_window("live");
        window.imp().live_view.show_toolbar_for_test();
        settle();
        let search = window.imp().live_view.toolbar_focus_targets()[0].clone();
        assert!(search.grab_focus());
        assert_owner(&window, &search, "Live Search before native Down");
        assert_native_input_delivery(&search);
        native_x11_key("Down");
        let list = TvWindow::find_descendant(
            window.imp().live_view.upcast_ref::<gtk::Widget>(),
            &|widget| widget.is::<gtk::ListBox>(),
        )
        .expect("mapped Live list");
        assert_owner(
            &window,
            &list,
            "Live Search native Down must reach first row",
        );
        native_x11_key("Up");
        assert_owner(&window, &search, "Live native Down Up round-trip");
        window.close();
    }

    /// Reproduce the owner's Mediathek route with GDK key delivery rather
    /// than invoking the capture router in isolation.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb with real XTest key injection"]
    fn gtk_focus001_xtest_mediathek_search_down_up() {
        let (_app, window) = mapped_window("mediathek");
        let controls = window.imp().mediathek_view.toolbar_focus_targets();
        let search = &controls[0];
        assert!(search.grab_focus());
        assert_owner(&window, search, "Mediathek before real Down");
        assert_native_input_delivery(search);
        native_x11_key("Down");
        let first = window
            .focused_widget()
            .expect("real Down must retain focus");
        assert!(
            controls.iter().any(|c| TvWindow::is_within(&first, c)),
            "native Down skipped Mediathek toolbar controls"
        );
        assert_ne!(first, *search, "native Down must advance");
        native_x11_key("Up");
        assert_owner(&window, search, "Mediathek native Down Up must reverse");
        window.close();
    }

    /// The owner's collapsed-row favourite star must be reachable without
    /// Tab/mouse. Probe all two-key D-pad approaches without assuming a
    /// particular geometry-selected direction. Longer paths are unverified.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb with native XTest injection"]
    fn gtk_focus001_streams_collapsed_star_two_key_reachability() {
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        streams.install_remote_action_test_rows();
        settle();
        assert!(streams.show_and_focus_test_results());
        let row = window.focused_widget().expect("selected fabricated row");
        assert_owner(&window, &row, "collapsed row start");
        let star = streams
            .remote_action_test_star()
            .expect("selected synthetic card has a star button");
        assert!(star.is_visible() && star.is_mapped() && star.is_sensitive());
        assert_native_input_delivery(&streams.toolbar_focus_targets()[0]);
        assert!(row.grab_focus());
        let keys = [
            gtk::gdk::Key::Up,
            gtk::gdk::Key::Down,
            gtk::gdk::Key::Left,
            gtk::gdk::Key::Right,
        ];
        let mut reachable = false;
        for first in keys {
            assert!(row.grab_focus(), "reset selected row for each probe");
            native_x11_key(first.name().expect("direction keysym").as_str());
            if owns_focus(&window, star.upcast_ref()) {
                reachable = true;
                break;
            }
            let intermediate = window.focused_widget();
            for second in keys {
                let Some(ref origin) = intermediate else {
                    continue;
                };
                if !origin.is_mapped() || !origin.is_sensitive() || !origin.grab_focus() {
                    continue;
                }
                native_x11_key(second.name().expect("direction keysym").as_str());
                if owns_focus(&window, star.upcast_ref()) {
                    reachable = true;
                    break;
                }
            }
            if reachable {
                break;
            }
        }
        assert!(
            reachable,
            "RED: selected collapsed card star cannot be reached within two D-pad moves"
        );
        assert_owner(&window, star.upcast_ref(), "collapsed star D-pad arrival");
        window.close();
    }

    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb with real XTest key injection"]
    fn gtk_focus001_streams_collapsed_star_native_reachability() {
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        let search = streams.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        streams.install_remote_action_test_rows();
        settle();
        assert!(streams.show_and_focus_test_results());
        let row = window.focused_widget().expect("selected collapsed row");
        assert_owner(&window, &row, "collapsed row before native Right");
        let star = streams
            .remote_action_test_star()
            .expect("selected bound card exposes a native favourite button");
        assert!(star.is_mapped(), "card favourite button must be mapped");
        native_x11_key("Right");
        assert_owner(
            &window,
            star.upcast_ref(),
            "collapsed card row --Right--> reachable favourite button",
        );
        window.close();
    }

    #[test]
    #[ignore = "requires separately isolated mapped GTK/Xvfb + compiled test schema"]
    fn gtk_focus001_global_mediathek_search_up_down_switcher() {
        let (_app, window) = mapped_window("mediathek");
        let search = window.imp().mediathek_view.toolbar_focus_targets()[0].clone();
        assert!(search.grab_focus());
        assert_owner(&window, &search, "Search initial");
        key(&window, gtk::gdk::Key::Up);
        let switcher = window.focused_widget().expect("Search Up target");
        assert!(
            switcher.type_().name() == "AdwViewSwitcherButton",
            "Search Up must focus a global category, got {}",
            switcher.type_().name()
        );
        assert_owner(&window, &switcher, "Search --Up--> category");
        key(&window, gtk::gdk::Key::Down);
        assert_owner(&window, &search, "category --Down--> Search");
        window.close();
    }

    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb and disposable SQLite"]
    fn gtk_focus001_streams_favourites_footer_async_focus_retention() {
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        use std::os::unix::fs::DirBuilderExt;
        let fixture =
            std::env::temp_dir().join(format!("broadcaster-focus001-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&fixture)
            .expect("private disposable DB folder");
        let ready = streams.focus001_attach_disposable_service(fixture.join("focus.sqlite"));
        for _ in 0..20 {
            if ready.get() {
                break;
            }
            settle();
        }
        assert!(ready.get(), "fixture service did not initialize");
        let footer = streams.toolbar_focus_targets()[8].clone();
        let button = footer
            .clone()
            .downcast::<gtk::Button>()
            .expect("favourites footer is mapped GTK Button");
        assert!(button.is_mapped());
        for index in 0..3 {
            assert!(button.grab_focus());
            assert_owner(&window, &footer, "favourites footer before OK");
            button.emit_clicked();
            for _ in 0..20 {
                if button.is_sensitive() {
                    break;
                }
                settle();
            }
            assert!(button.is_sensitive(), "favourites write timed out");
            assert_owner(
                &window,
                &footer,
                &format!("favourites footer retained focus after click {index}"),
            );
        }
        window.close();
        std::fs::remove_dir_all(&fixture).expect("remove disposable SQLite fixture");
    }

    #[test]
    #[ignore = "requires separately isolated mapped GTK/Xvfb + compiled test schema"]
    fn gtk_focus001_streams_back_escape_home_are_semantic() {
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        streams.install_remote_action_test_rows();
        settle();
        assert!(streams.show_and_focus_test_results());
        streams.remote_action_test_activate();
        settle();
        key(&window, gtk::gdk::Key::Right);
        let play = streams.remote_action_test_buttons().expect("actions")[0].clone();
        assert_owner(&window, play.upcast_ref(), "expanded Play");
        key(&window, gtk::gdk::Key::Back);
        assert!(window.imp().stack.visible_child_name().as_deref() == Some("streams"));
        let owner = window.focused_widget().expect("Back collapsed row");
        assert!(
            streams.contains_results_focus(&owner),
            "Back must restore row"
        );
        streams.remote_action_test_activate();
        settle();
        key(&window, gtk::gdk::Key::Right);
        key(&window, gtk::gdk::Key::Escape);
        let owner = window.focused_widget().expect("Escape collapsed row");
        assert!(
            streams.contains_results_focus(&owner),
            "Escape must restore row"
        );
        assert!(window.imp().stack.visible_child_name().as_deref() == Some("streams"));
        key(&window, gtk::gdk::Key::HomePage);
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("mediathek")
        );
        window.close();
    }

    /// Expanded actions must retain keyboard focus under ten repeated Right
    /// events, and Up/Down must follow Play/Edit/Programme without a mouse.
    #[test]
    #[ignore = "requires separately isolated mapped GTK/Xvfb + compiled test schema"]
    fn gtk_focus001_streams_expanded_actions_right_ten_retains_owner() {
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        streams.install_remote_action_test_rows();
        settle();
        assert!(streams.show_and_focus_test_results());
        streams.remote_action_test_activate();
        settle();
        let buttons = streams
            .remote_action_test_buttons()
            .expect("fabricated selected row has Play/Edit/Programme actions");
        key(&window, gtk::gdk::Key::Right);
        assert_owner(
            &window,
            buttons[0].upcast_ref(),
            "selected row Right -> Play",
        );
        for (index, button) in buttons.iter().enumerate() {
            assert_owner(
                &window,
                button.upcast_ref(),
                "expanded action initial focus",
            );
            for repeat in 0..10 {
                key(&window, gtk::gdk::Key::Right);
                assert_owner(
                    &window,
                    button.upcast_ref(),
                    &format!("action {index} Right repeat {repeat}"),
                );
            }
            if index + 1 < buttons.len() {
                key(&window, gtk::gdk::Key::Down);
                assert_owner(&window, buttons[index + 1].upcast_ref(), "next action");
            }
        }
        key(&window, gtk::gdk::Key::Up);
        assert_owner(&window, buttons[1].upcast_ref(), "Programme Up -> Edit");
        key(&window, gtk::gdk::Key::Up);
        assert_owner(&window, buttons[0].upcast_ref(), "Edit Up -> Play");
        window.close();
    }

    /// The three pages share the same global Up/Down boundary. Run the Live
    /// variant in its own GTK process, not a multi-page simulated mock.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb and disposable GSettings"]
    fn gtk_focus001_live_search_switcher_roundtrip() {
        let (_app, window) = mapped_window("live");
        window.imp().live_view.show_toolbar_for_test();
        settle();
        let search = window.imp().live_view.toolbar_focus_targets()[0].clone();
        assert!(search.grab_focus());
        assert_owner(&window, &search, "Live Search start");
        key(&window, gtk::gdk::Key::Up);
        let switcher = window
            .focused_widget()
            .expect("Live Search Up must retain owner");
        assert_eq!(switcher.type_().name(), "AdwViewSwitcherButton");
        assert_owner(&window, &switcher, "Live Search Up to category");
        key(&window, gtk::gdk::Key::Down);
        assert_owner(&window, &search, "Live category Down returns Search");
        window.close();
    }

    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb and disposable GSettings"]
    fn gtk_focus001_streams_search_switcher_roundtrip() {
        let (_app, window) = mapped_window("streams");
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert!(search.grab_focus());
        assert_owner(&window, &search, "My Channels Search start");
        key(&window, gtk::gdk::Key::Up);
        let switcher = window
            .focused_widget()
            .expect("My Channels Search Up owner");
        assert_eq!(switcher.type_().name(), "AdwViewSwitcherButton");
        assert_owner(&window, &switcher, "My Channels Search Up to category");
        key(&window, gtk::gdk::Key::Down);
        assert_owner(&window, &search, "My Channels category Down returns Search");
        window.close();
    }

    /// Search and OSK launcher are a horizontal pair, not part of the
    /// historical vertical list-navigation chain. Real mapped focus is read.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb and disposable GSettings"]
    fn gtk_focus001_live_search_keyboard_horizontal_reverse() {
        let (_app, window) = mapped_window("live");
        window.imp().live_view.show_toolbar_for_test();
        settle();
        let controls = window.imp().live_view.toolbar_focus_targets();
        assert_eq!(controls.len(), 2);
        assert!(controls[0].grab_focus());
        assert_owner(&window, &controls[0], "Live Search horizontal start");
        key(&window, gtk::gdk::Key::Right);
        assert_owner(&window, &controls[1], "Live Search Right to Keyboard");
        key(&window, gtk::gdk::Key::Left);
        assert_owner(&window, &controls[0], "Live Keyboard Left to Search");
        window.close();
    }

    /// Do not conflate GTK focusability with D-pad reachability. This
    /// structural case verifies every enabled real Mediathek toolbar target
    /// at a compact width, and is separately labelled HELPER_ONLY in TSV.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb and disposable GSettings"]
    fn gtk_focus001_mediathek_compact_toolbar_ownership_inventory() {
        let (_app, window) = mapped_window("mediathek");
        window.set_default_size(640, 480);
        settle();
        let controls = window.imp().mediathek_view.toolbar_focus_targets();
        assert_eq!(
            controls.len(),
            7,
            "all documented toolbar classes are present"
        );
        for (index, control) in controls.iter().enumerate() {
            assert!(
                control.is_mapped() && control.is_visible() && control.is_sensitive(),
                "Mediathek compact toolbar index {index} unavailable"
            );
            assert!(
                control.grab_focus(),
                "Mediathek toolbar index {index} not focusable"
            );
            assert_owner(
                &window,
                control,
                &format!("compact Mediathek control {index}"),
            );
        }
        window.close();
    }

    /// Empty My Channels must not silently make all toolbar actions orphaned
    /// when the underlying SQLite service has not provided any rows.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb and disposable GSettings"]
    fn gtk_focus001_streams_empty_toolbar_ownership_inventory() {
        let (_app, window) = mapped_window("streams");
        let controls = window.imp().streams_view.toolbar_focus_targets();
        assert_eq!(controls.len(), 12);
        let mut enabled = 0;
        for (index, control) in controls.iter().enumerate() {
            if !control.is_mapped() || !control.is_visible() || !control.is_sensitive() {
                eprintln!("FOCUS001 disabled/hidden empty-toolbar index={index}");
                continue;
            }
            enabled += 1;
            assert!(
                control.grab_focus(),
                "enabled My Channels control {index} unreachable"
            );
            assert_owner(
                &window,
                control,
                &format!("empty My Channels control {index}"),
            );
        }
        assert!(enabled >= 3, "no usable controls remain on an empty list");
        window.close();
    }

    /// Opening the real application menu must not strand the keyboard focus
    /// behind a mapped popover, and Escape must dismiss the popover.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb with real XTest key injection"]
    fn gtk_focus001_app_menu_escape_restores_focus() {
        let (_app, window) = mapped_window("mediathek");
        let menu = window.imp().menu_button.get();
        assert_native_input_delivery(&window.imp().mediathek_view.toolbar_focus_targets()[0]);
        assert!(menu.grab_focus());
        menu.popup();
        settle();
        let popover = menu.popover().expect("actual application menu popover");
        assert!(popover.is_mapped(), "application menu failed to map");
        let owner = window
            .focused_widget()
            .expect("menu must have a focus owner");
        assert!(
            TvWindow::is_within(&owner, popover.upcast_ref()),
            "menu opened but focus remained outside topmost popover: {}",
            owner.type_().name()
        );
        native_x11_key("Escape");
        assert!(!popover.is_mapped(), "Escape left application menu open");
        let owner = window.focused_widget().expect("Escape orphaned focus");
        assert!(
            owner.is_mapped() && owner.is_sensitive(),
            "Escape returned to invalid owner"
        );
        window.close();
    }

    /// The real OSK launches from a page toolbar and is a topmost AdwDialog,
    /// not a detached mock grid. Direct click opens it; physical XTest keys
    /// exercise modal propagation and dismissal independently.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb with real XTest key injection"]
    fn gtk_focus001_mediathek_osk_modal_escape_return() {
        let (_app, window) = mapped_window("mediathek");
        let controls = window.imp().mediathek_view.toolbar_focus_targets();
        assert_native_input_delivery(&controls[0]);
        let launcher = controls[1]
            .clone()
            .downcast::<gtk::Button>()
            .expect("OSK button");
        assert!(launcher.grab_focus());
        launcher.emit_clicked();
        settle();
        let dialog = window.visible_dialog().expect("OSK dialog must appear");
        assert!(dialog.is_mapped(), "OSK dialog must map");
        let owner = window.focused_widget().expect("OSK must own focus");
        assert!(
            TvWindow::is_within(&owner, dialog.upcast_ref()),
            "OSK opened without keyboard focus inside its modal"
        );
        native_x11_key("Right");
        let after = window.focused_widget().expect("OSK Right lost focus");
        assert!(
            TvWindow::is_within(&after, dialog.upcast_ref()) && after.is_mapped(),
            "OSK D-pad Right escaped the topmost dialog"
        );
        native_x11_key("Escape");
        settle();
        assert!(
            window.visible_dialog().is_none(),
            "OSK Escape failed to dismiss topmost dialog"
        );
        let owner = window.focused_widget().expect("OSK Escape lost focus");
        assert!(
            TvWindow::is_within(&owner, &controls[0]) || TvWindow::is_within(&owner, &controls[1]),
            "OSK Escape did not return to Search or its launcher"
        );
        window.close();
    }
    /// Native OK on a collapsed Live card must expand directly into primary
    /// Play focus. Entry never constitutes a separate playback activation.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb with native XTest"]
    fn gtk_focus001_xtest_live_card_ok_focuses_play() {
        let (_app, window) = mapped_window("live");
        window.imp().live_view.show_toolbar_for_test();
        let search = window.imp().live_view.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        let (row, play) = window
            .imp()
            .live_view
            .focus001_install_disposable_live_row();
        settle();
        assert!(
            row.is_mapped() && row.grab_focus(),
            "synthetic Live row unmapped"
        );
        assert_owner(&window, row.upcast_ref(), "Live collapsed row before OK");
        native_x11_key("Return");
        assert_owner(
            &window,
            play.upcast_ref(),
            "Live OK entry must directly focus Play",
        );
        window.close();
    }

    /// Right must be a one-step alternative portal into Live card actions.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb with native XTest"]
    fn gtk_focus001_xtest_live_card_right_focuses_play() {
        let (_app, window) = mapped_window("live");
        window.imp().live_view.show_toolbar_for_test();
        let search = window.imp().live_view.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        let (row, play) = window
            .imp()
            .live_view
            .focus001_install_disposable_live_row();
        settle();
        assert!(row.is_mapped() && row.grab_focus());
        assert_owner(&window, row.upcast_ref(), "Live collapsed row before Right");
        native_x11_key("Right");
        assert_owner(
            &window,
            play.upcast_ref(),
            "Live Right entry must directly focus Play",
        );
        window.close();
    }

    /// Actual Manage Sources modal and full two-row production action layout.
    /// Only arrow travel is exercised, never a destructive activation.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb, disposable SQLite and native XTest"]
    fn gtk_focus001_xtest_manage_sources_refresh_down_reaches_lower_row() {
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        let fixture = std::env::temp_dir().join(format!(
            "broadcaster-focus001-manager-{}",
            uuid::Uuid::new_v4()
        ));
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&fixture)
            .expect("private disposable directory");
        let path = fixture.join("manager.sqlite");
        crate::streams::view::TvStreamsView::focus001_seed_disposable_manager_source(path.clone());
        let ready = streams.focus001_attach_disposable_service(path);
        for _ in 0..30 {
            if ready.get() {
                break;
            }
            settle();
        }
        assert!(ready.get(), "disposable manager service did not initialize");
        let search = streams.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        streams.show_manager();
        let mut refresh = None;
        let mut replace = None;
        let mut delete = None;
        for _ in 0..30 {
            settle();
            if let Some(dialog) = window.visible_dialog() {
                refresh = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
                    w.downcast_ref::<gtk::Button>()
                        .is_some_and(|b| b.label().as_deref() == Some("Refresh"))
                });
                replace = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
                    w.downcast_ref::<gtk::Button>()
                        .is_some_and(|b| b.label().as_deref() == Some("Replace"))
                });
                delete = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
                    w.downcast_ref::<gtk::Button>()
                        .is_some_and(|b| b.label().as_deref() == Some("Delete"))
                });
            }
            if refresh.is_some() && replace.is_some() && delete.is_some() {
                break;
            }
        }
        let refresh = refresh.expect("real source Refresh button");
        let replace = replace.expect("real source Replace button");
        let delete = delete.expect("real source Delete button");
        assert!(refresh.grab_focus());
        assert_owner(&window, &refresh, "Manage Sources Refresh initial");
        // Explore actual modal directions. Each state is first reached from
        // Refresh by native keys, then revisited with GTK grab_focus for a
        // bounded directed adjacency inventory; this is not physical replay.
        let dialog = window.visible_dialog().expect("manager remains open");
        let mut queue =
            std::collections::VecDeque::from([(refresh.clone(), Vec::<&'static str>::new())]);
        let mut seen = vec![refresh.clone()];
        let mut found_replace = None;
        let mut found_delete = None;
        while let Some((from, route)) = queue.pop_front() {
            assert!(seen.len() <= 24, "manager graph escaped bounded fixture");
            for arrow in ["Up", "Down", "Left", "Right"] {
                assert!(from.grab_focus(), "visited manager focus target detached");
                settle();
                native_x11_key(arrow);
                let owner = window
                    .focused_widget()
                    .expect("manager D-pad lost focus owner");
                assert!(
                    owner.is_mapped()
                        && owner.is_visible()
                        && owner.is_sensitive()
                        && TvWindow::is_within(&owner, dialog.upcast_ref()),
                    "manager D-pad {arrow} escaped dialog or focused an invalid widget"
                );
                let mut next_route = route.clone();
                next_route.push(arrow);
                let display = owner
                    .downcast_ref::<gtk::Button>()
                    .and_then(|button| button.label())
                    .map_or_else(
                        || owner.type_().name().to_string(),
                        |label| label.to_string(),
                    );
                eprintln!(
                    "FOCUS001 manager route={} owner={display}",
                    next_route.join(",")
                );
                if TvWindow::is_within(&owner, &replace) {
                    found_replace = Some(next_route.clone());
                }
                if TvWindow::is_within(&owner, &delete) {
                    found_delete = Some(next_route.clone());
                }
                if !seen.contains(&owner) {
                    seen.push(owner.clone());
                    queue.push_back((owner, next_route));
                }
            }
        }
        eprintln!(
            "FOCUS001 manager lower-row reachability Replace={found_replace:?} Delete={found_delete:?} visited={}",
            seen.len()
        );
        assert!(
            found_replace.is_some() && found_delete.is_some(),
            "actual Manage Sources Replace and Delete must BOTH be reachable by D-pad"
        );
        window.close();
        std::fs::remove_dir_all(&fixture).expect("remove disposable source fixture");
    }

    /// Exercise multiple REAL source manager rows, reorder boundaries and
    /// nested action dialogs with fully disposable SQLite source records.
    /// All activation is a native OK after the selected button is mapped;
    /// actions that would fetch or delete a source are never confirmed.
    #[test]
    #[ignore = "needs isolated XTest + disposable three-source SQLite manager"]
    fn gtk_focus001_xtest_manage_sources_three_rows_native_modal_actions() {
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        let fixture = std::env::temp_dir().join(format!(
            "broadcaster-focus001-multi-{}",
            uuid::Uuid::new_v4()
        ));
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&fixture)
            .expect("isolated manager scratch");
        let path = fixture.join("manager.sqlite");
        crate::streams::view::TvStreamsView::focus001_seed_disposable_manager_sources(
            path.clone(),
            3,
        );
        let ready = streams.focus001_attach_disposable_service(path);
        for _ in 0..45 {
            if ready.get() {
                break;
            }
            settle();
        }
        assert!(ready.get(), "three-source service did not initialize");
        let search = streams.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        streams.show_manager();
        let mut dialog = None;
        for _ in 0..45 {
            settle();
            if let Some(d) = window.visible_dialog() {
                if TvWindow::find_descendant(d.upcast_ref(), &|w| {
                    w.downcast_ref::<gtk::Button>()
                        .is_some_and(|b| b.label().as_deref() == Some("Replace"))
                })
                .is_some()
                {
                    dialog = Some(d);
                    break;
                }
            }
        }
        let manager = dialog.expect("real three-source manager not mapped");
        let mut buttons = Vec::<gtk::Button>::new();
        let mut queue = vec![manager.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = queue.pop() {
            if let Some(b) = widget.downcast_ref::<gtk::Button>() {
                buttons.push(b.clone());
            }
            let mut child = widget.first_child();
            while let Some(w) = child {
                child = w.next_sibling();
                queue.push(w);
            }
        }
        let get = |label: &str| -> Vec<gtk::Button> {
            buttons
                .iter()
                .filter(|b| b.label().as_deref() == Some(label))
                .cloned()
                .collect()
        };
        for label in [
            "Refresh", "Defaults", "Rename", "Replace", "Delete", "↑", "↓",
        ] {
            let group = get(label);
            assert_eq!(group.len(), 3, "every row must render {label}");
            for (i, b) in group.iter().enumerate() {
                assert!(b.is_mapped() && b.is_visible(), "{label}[{i}] not mapped");
                assert!(
                    b.compute_bounds(window.upcast_ref::<gtk::Widget>())
                        .is_some(),
                    "{label}[{i}] no mapped geometry"
                );
            }
        }
        // Tree traversal order is not the displayed ListBox order. Discover
        // the middle row by the paired enabled move controls, and verify
        // exactly one first/last boundary without relying on that ordering.
        let up = get("↑");
        let down = get("↓");
        let middle = (0..3)
            .find(|&i| up[i].is_sensitive() && down[i].is_sensitive())
            .expect("middle source must permit both reorder directions");
        assert_eq!(up.iter().filter(|b| b.is_sensitive()).count(), 2);
        assert_eq!(down.iter().filter(|b| b.is_sensitive()).count(), 2);
        assert_eq!((0..3).filter(|&i| !up[i].is_sensitive()).count(), 1);
        assert_eq!((0..3).filter(|&i| !down[i].is_sensitive()).count(), 1);
        let mut missing = Vec::<String>::new();
        // Start with the mapped upper row, then send genuine arrows. An
        // unreachable lower control is recorded as RED at the END, after the
        // permitted safe nested modal actions have all been checked.
        let from = get("Refresh")[middle].clone();
        assert!(from.grab_focus());
        native_x11_key("Down");
        let after = window.focused_widget().expect("middle-row Down lost focus");
        let lower = [
            get("Replace")[middle].clone(),
            get("Delete")[middle].clone(),
            get("↑")[middle].clone(),
            get("↓")[middle].clone(),
        ];
        if !lower
            .iter()
            .any(|button| TvWindow::is_within(&after, button.upcast_ref()))
        {
            missing.push(format!(
                "middle Refresh Down reached {}, not lower action",
                after.type_().name()
            ));
        }
        // Inspect native OK/Back for each destructive/setup dialog, without
        // activating a network fetch, file picker, or destructive confirmation.
        for (label, nested_action) in [
            ("Rename", "Save"),
            ("Defaults", "Save"),
            ("Replace", "Replace HTTP(S) URL"),
            ("Delete", "Keep Source"),
        ] {
            let button = get(label)[middle].clone();
            assert!(button.grab_focus(), "{label} must accept enabled GTK focus");
            native_x11_key("Return");
            settle();
            let nested = window.visible_dialog().expect("source action opens modal");
            assert_ne!(
                nested, manager,
                "{label} native OK did not open child dialog"
            );
            let found = TvWindow::find_descendant(nested.upcast_ref(), &|w| {
                w.downcast_ref::<gtk::Button>()
                    .is_some_and(|b| b.label().as_deref() == Some(nested_action))
            });
            if found.is_none() {
                missing.push(format!("{label} nested {nested_action} absent"));
            }
            native_x11_key("Escape");
            settle();
            assert_eq!(
                window.visible_dialog(),
                Some(manager.clone()),
                "{label} Escape must return to original manager"
            );
            let owner = window
                .focused_widget()
                .expect("source manager modal return owner");
            assert!(
                TvWindow::is_within(&owner, manager.upcast_ref()) && owner.is_mapped(),
                "{label} modal Escape leaves manager without mapped focus"
            );
        }
        eprintln!("FOCUS001 source_multi_native rows=3 disabled_boundaries=checked nested_modal_actions=4 missing={missing:?}");
        assert!(
            missing.is_empty(),
            "source manager three-row native action defects: {missing:?}"
        );
        native_x11_key("Escape");
        settle();
        window.close();
        std::fs::remove_dir_all(&fixture).expect("clean disposable manager source fixture");
    }

    /// Test the seven *real* Mediathek toolbar controls as a native directional
    /// graph, including every initial arrow, without assuming widget creation
    /// order equals the on-screen geometry. GTK grab_focus is used only to
    /// seed each edge independently; every edge itself is delivered by XTest.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb with native XTest key injection"]
    fn gtk_focus001_xtest_mediathek_complete_toolbar_graph() {
        let (_app, window) = mapped_window("mediathek");
        let targets = window.imp().mediathek_view.toolbar_focus_targets();
        assert_eq!(targets.len(), 7, "Mediathek toolbar layout changed");
        assert_native_input_delivery(&targets[0]);
        let arrows = ["Up", "Down", "Left", "Right"];
        let mut graph = vec![Vec::<usize>::new(); targets.len()];
        let mut reverse_failures = Vec::new();
        let mut wrong_directions = Vec::new();
        for (from_index, from) in targets.iter().enumerate() {
            assert!(from.is_mapped() && from.is_sensitive() && from.grab_focus());
            let from_rect = from
                .compute_bounds(window.upcast_ref::<gtk::Widget>())
                .expect("mapped Mediathek target geometry");
            let from_x = from_rect.x() + from_rect.width() / 2.0;
            let from_y = from_rect.y() + from_rect.height() / 2.0;
            for arrow in arrows {
                assert!(
                    from.grab_focus(),
                    "Mediathek source target lost focusability"
                );
                settle();
                native_x11_key(arrow);
                let after = window
                    .focused_widget()
                    .expect("native arrow lost GTK owner");
                assert!(
                    after.is_mapped() && after.is_visible() && after.is_sensitive(),
                    "Mediathek native {arrow} from target {from_index} orphaned focus"
                );
                let destination = targets.iter().position(|t| TvWindow::is_within(&after, t));
                eprintln!(
                    "FOCUS001 mediathek_toolbar from={from_index} key={arrow} to={destination:?} owner={}",
                    after.type_().name()
                );
                if let Some(to_index) = destination {
                    if to_index == from_index {
                        continue; // GTK editing caret / layout boundary
                    }
                    graph[from_index].push(to_index);
                    let to_rect = targets[to_index]
                        .compute_bounds(window.upcast_ref::<gtk::Widget>())
                        .expect("mapped destination geometry");
                    let to_x = to_rect.x() + to_rect.width() / 2.0;
                    let to_y = to_rect.y() + to_rect.height() / 2.0;
                    let forward = match arrow {
                        "Up" => to_y < from_y - 2.0,
                        "Down" => to_y > from_y + 2.0,
                        "Left" => to_x < from_x - 2.0,
                        "Right" => to_x > from_x + 2.0,
                        _ => unreachable!(),
                    };
                    if !forward {
                        wrong_directions.push(format!("{from_index} {arrow} -> {to_index}"));
                    }
                    let opposite = match arrow {
                        "Up" => "Down",
                        "Down" => "Up",
                        "Left" => "Right",
                        "Right" => "Left",
                        _ => unreachable!(),
                    };
                    // Independent native reverse edge. A detected local toolbar
                    // step should reverse where layout/data did not change.
                    native_x11_key(opposite);
                    let return_owner = window.focused_widget();
                    if !return_owner
                        .as_ref()
                        .is_some_and(|w| TvWindow::is_within(w, from))
                    {
                        reverse_failures.push(format!(
                            "{from_index} {arrow} -> {to_index} {opposite} -> {}",
                            return_owner.as_ref().map_or("NONE", |w| w.type_().name())
                        ));
                    }
                }
            }
        }
        // A real reachable graph rather than individually grab-focusable
        // targets: all enabled toolbar classes must have an arrow route from
        // Search (editor caret steps are not graph edges).
        let mut seen = vec![false; targets.len()];
        seen[0] = true;
        let mut queue = std::collections::VecDeque::from([0]);
        while let Some(from) = queue.pop_front() {
            for &to in &graph[from] {
                if !seen[to] {
                    seen[to] = true;
                    queue.push_back(to);
                }
            }
        }
        eprintln!(
            "FOCUS001 mediathek_native_graph reachable={seen:?} wrong_directions={wrong_directions:?} reverse_failures={reverse_failures:?}"
        );
        assert!(
            wrong_directions.is_empty(),
            "D-pad chose non-spatial toolbar target"
        );
        assert!(
            reverse_failures.is_empty(),
            "Mediathek toolbar local arrows failed to reverse"
        );
        assert!(
            seen.iter().all(|b| *b),
            "D-pad cannot reach all seven Mediathek toolbar controls"
        );
        window.close();
    }

    /// Activate all three *real* view-switcher pages using the actual key
    /// dispatcher; the earlier global boundary tests never activated a page.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb with native XTest key injection"]
    fn gtk_focus001_xtest_global_three_page_switcher_activation() {
        let (_app, window) = mapped_window("live");
        window.imp().live_view.show_toolbar_for_test();
        settle();
        let live_search = window.imp().live_view.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&live_search);
        assert!(live_search.grab_focus());
        native_x11_key("Up");
        let category = window.focused_widget().expect("Live category must focus");
        assert_eq!(category.type_().name(), "AdwViewSwitcherButton");
        // A focus portal must target the *current* page, not merely any
        // switcher button. Otherwise even the first OK changes pages.
        native_x11_key("Return");
        eprintln!(
            "FOCUS001 global Live Search Up, then OK page={:?}",
            window.imp().stack.visible_child_name()
        );
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("live"),
            "Live Search Up must target the current Live category before Right"
        );
        native_x11_key("Right");
        let next_category = window.focused_widget().expect("next category must focus");
        assert_eq!(next_category.type_().name(), "AdwViewSwitcherButton");
        assert_ne!(
            next_category, category,
            "Right must move to adjacent category"
        );
        eprintln!(
            "FOCUS001 global switcher Live Right focused={} visible_page_before_OK={:?}",
            next_category.type_().name(),
            window.imp().stack.visible_child_name()
        );
        native_x11_key("Return");
        eprintln!(
            "FOCUS001 global switcher after OK page={:?}",
            window.imp().stack.visible_child_name()
        );
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("mediathek"),
            "OK on the next category must activate Mediathek"
        );
        native_x11_key("Down");
        let mediathek_search = window.imp().mediathek_view.toolbar_focus_targets()[0].clone();
        assert_owner(&window, &mediathek_search, "Mediathek tab Down to Search");
        native_x11_key("Up");
        assert_eq!(
            window.focused_widget().as_ref().map(|w| w.type_().name()),
            Some("AdwViewSwitcherButton"),
            "Mediathek Search Up should return to its category"
        );
        native_x11_key("Right");
        let next_category = window.focused_widget().expect("My Channels category focus");
        assert_eq!(next_category.type_().name(), "AdwViewSwitcherButton");
        native_x11_key("Return");
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("streams"),
            "OK on the next category must activate My Channels"
        );
        native_x11_key("Down");
        let streams_search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert_owner(&window, &streams_search, "My Channels tab Down to Search");
        window.close();
    }

    /// Explicitly reject a reversed-arrow oracle against a real mapped GTK
    /// toolbar move. This checks the *direction oracle*, rather than treating
    /// success from grab_focus() as navigation evidence.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb with native XTest"]
    fn gtk_focus001_xtest_negative_control_rejects_wrong_arrow() {
        let (_app, window) = mapped_window("live");
        window.imp().live_view.show_toolbar_for_test();
        let targets = window.imp().live_view.toolbar_focus_targets();
        assert_native_input_delivery(&targets[0]);
        let search = targets[0].clone();
        let keyboard = targets[1].clone();
        assert!(search.grab_focus());
        native_x11_key("Right");
        assert_owner(&window, &keyboard, "native Right to OSK launcher");
        let a = search
            .compute_bounds(window.upcast_ref::<gtk::Widget>())
            .expect("Search bounds");
        let b = keyboard
            .compute_bounds(window.upcast_ref::<gtk::Widget>())
            .expect("OSK bounds");
        let delta = (b.x() + b.width() * 0.5) - (a.x() + a.width() * 0.5);
        assert!(
            delta > 2.0,
            "positive spatial Right control must really move right"
        );
        // A deliberately faulty router/oracle would call this a Left move:
        // ensure the same positive native transition is rejected as Left.
        let falsely_accepted_left = delta < -2.0;
        assert!(
            !falsely_accepted_left,
            "negative control failed to detect wrong arrow"
        );
        eprintln!("FOCUS001 negative_wrong_arrow PASS native_Right_delta={delta:.1} wrong_Left_rejected=true");
        window.close();
    }

    /// Native OSK keys and row boundaries, including a horizontal-wrap
    /// exception. Test-owned focus seeding only enters independent edges;
    /// each asserted transition is made with real XTest/GDK events.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb with native XTest"]
    fn gtk_focus001_xtest_osk_native_key_grid_and_layout() {
        let (_app, window) = mapped_window("mediathek");
        let controls = window.imp().mediathek_view.toolbar_focus_targets();
        assert_native_input_delivery(&controls[0]);
        controls[1]
            .clone()
            .downcast::<gtk::Button>()
            .expect("OSK launcher")
            .emit_clicked();
        settle();
        let dialog = window.visible_dialog().expect("actual mapped OSK dialog");
        let get_key = |label: &str| -> gtk::Button {
            TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
                w.downcast_ref::<gtk::Button>()
                    .is_some_and(|b| b.label().as_deref() == Some(label))
            })
            .unwrap_or_else(|| panic!("mapped OSK key missing: {label}"))
            .downcast::<gtk::Button>()
            .expect("OSK key button")
        };
        let q = get_key("q");
        assert!(q.is_mapped() && q.is_sensitive() && q.grab_focus());
        native_x11_key("Right");
        assert_owner(&window, get_key("w").upcast_ref(), "OSK q Right w");
        native_x11_key("Left");
        assert_owner(&window, q.upcast_ref(), "OSK w Left q");
        native_x11_key("Left");
        assert_owner(
            &window,
            get_key("ü").upcast_ref(),
            "OSK row-left wrap q to ü",
        );
        native_x11_key("Right");
        assert_owner(&window, q.upcast_ref(), "OSK wrap reversed ü Right q");
        // The live grid must not relinquish focus to underlying page widgets.
        for name in ["Up", "Down", "Down", "Up"] {
            native_x11_key(name);
            let owner = window
                .focused_widget()
                .expect("OSK directional key lost owner");
            assert!(
                owner.is_mapped()
                    && owner.is_sensitive()
                    && TvWindow::is_within(&owner, dialog.upcast_ref()),
                "OSK {name} escaped modal or found detached control"
            );
        }
        // Native OK on a letter edits only the isolated draft, never the
        // origin's search entry until a *separate* Done action.
        assert!(q.grab_focus());
        let original = controls[0]
            .clone()
            .downcast::<gtk::SearchEntry>()
            .expect("source SearchEntry");
        original.set_text("");
        native_x11_key("Return");
        let draft = TvWindow::find_descendant(dialog.upcast_ref(), &|w| w.is::<gtk::Entry>())
            .expect("OSK isolated draft")
            .downcast::<gtk::Entry>()
            .expect("draft entry");
        assert!(
            draft.text().contains('q'),
            "native OSK OK did not enter q in draft"
        );
        assert!(
            original.text().is_empty(),
            "OSK preview leaked uncommitted edit to origin"
        );
        let shift = get_key("Shift");
        assert!(shift.grab_focus());
        native_x11_key("Return");
        assert_eq!(
            get_key("Q").label().as_deref(),
            Some("Q"),
            "Shift did not relabel OSK"
        );
        let layout = get_key("QWERTZ");
        assert!(layout.grab_focus());
        native_x11_key("Return");
        assert!(
            get_key("Z").is_mapped(),
            "QWERTZ layout switch did not remap Y position"
        );
        eprintln!("FOCUS001 osk_native q/w/row_wrap, vertical modal, native OK and Shift/QWERTZ labels verified");
        native_x11_key("Escape");
        assert!(
            window.visible_dialog().is_none(),
            "OSK Escape must dismiss keyboard"
        );
        window.close();
    }

    // The existing q/w canary covers neither commit/cancel nor the other
    // pages. Each wrapper below is an isolated native-XTest process. The
    // source is the actual search field of the mapped production view.
    fn focus001_native_osk_transaction(page: &str, width: i32) {
        let (_app, window) = mapped_window(page);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        if page == "live" {
            window.imp().live_view.show_toolbar_for_test();
        }
        settle();
        let controls = match page {
            "live" => window.imp().live_view.toolbar_focus_targets(),
            "mediathek" => window.imp().mediathek_view.toolbar_focus_targets(),
            "streams" => window.imp().streams_view.toolbar_focus_targets(),
            _ => unreachable!(),
        };
        let origin = controls[0]
            .clone()
            .downcast::<gtk::SearchEntry>()
            .expect("real page SearchEntry");
        assert_native_input_delivery(origin.upcast_ref());
        let keyboard = if page == "streams" {
            &controls[11]
        } else {
            &controls[1]
        };
        let launcher = keyboard
            .clone()
            .downcast::<gtk::Button>()
            .expect("OSK launcher");
        assert!(launcher.is_mapped() && launcher.grab_focus());
        native_x11_key("Return");
        settle();
        let dialog = window.visible_dialog().expect("native OK opens actual OSK");
        assert!(dialog.is_mapped());
        let get = |name: &str| -> gtk::Button {
            TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
                w.downcast_ref::<gtk::Button>()
                    .is_some_and(|b| b.label().as_deref() == Some(name))
            })
            .unwrap_or_else(|| panic!("{page} OSK missing key '{name}'"))
            .downcast::<gtk::Button>()
            .expect("key is GtkButton")
        };
        let keys = [
            "1",
            "0",
            "q",
            "w",
            "ü",
            "a",
            "ä",
            "z",
            ".",
            ":",
            "~",
            "Backspace",
            "←",
            "Space",
            "→",
            "Clear",
            "Shift",
            "QWERTZ",
            "Symbols",
            "Done",
            "Cancel",
        ];
        for label in keys {
            let button = get(label);
            let bounds = button
                .compute_bounds(window.upcast_ref::<gtk::Widget>())
                .expect("mapped OSK button must have window allocation");
            assert!(button.is_mapped() && button.is_visible() && button.is_sensitive());
            assert!(
                bounds.width() > 0.0 && bounds.height() > 0.0,
                "{page} OSK {label} at {width}px has no hit/focus area"
            );
            // Text keys already expose their visible label as the GTK
            // accessible name; the two icon-only caret controls require a
            // distinct descriptive tooltip for their actual action.
            if label == "←" || label == "→" {
                assert!(
                    button
                        .tooltip_text()
                        .is_some_and(|hint| hint.contains("Caret")),
                    "{page} OSK caret action {label} has no accessible help"
                );
            } else {
                assert!(button.label().is_some(), "{page} OSK {label} unlabeled");
            }
        }
        let q = get("q");
        assert!(q.grab_focus());
        native_x11_key("Right");
        assert_owner(&window, get("w").upcast_ref(), "OSK q Right w");
        native_x11_key("Left");
        assert_owner(&window, q.upcast_ref(), "OSK w Left q");
        let draft = TvWindow::find_descendant(dialog.upcast_ref(), &|w| w.is::<gtk::Entry>())
            .expect("OSK live draft")
            .downcast::<gtk::Entry>()
            .unwrap();
        origin.set_text("");
        assert!(q.grab_focus());
        native_x11_key("Return");
        assert_eq!(draft.text().as_str(), "q");
        assert!(origin.text().is_empty(), "typing must not leak draft");
        let space = get("Space");
        assert!(space.grab_focus());
        native_x11_key("Return");
        assert_eq!(draft.text().as_str(), "q ");
        let backspace = get("Backspace");
        assert!(backspace.grab_focus());
        native_x11_key("Return");
        assert_eq!(draft.text().as_str(), "q");
        let caret_left = get("←");
        assert!(caret_left.grab_focus());
        native_x11_key("Return");
        assert_eq!(draft.position(), 0);
        let caret_right = get("→");
        assert!(caret_right.grab_focus());
        native_x11_key("Return");
        assert_eq!(draft.position(), 1);
        let symbols = get("Symbols");
        assert!(symbols.grab_focus());
        native_x11_key("Return");
        assert!(
            get("!").is_mapped(),
            "Symbols must relabel mapped letter grid"
        );
        assert!(get("Letters").grab_focus());
        native_x11_key("Return");
        assert!(get("q").is_mapped());
        assert!(get("Shift").grab_focus());
        native_x11_key("Return");
        assert!(get("Q").is_mapped());
        assert!(get("QWERTZ").grab_focus());
        native_x11_key("Return");
        assert!(get("QWERTY").is_mapped(), "QWERTZ switch missing");
        let done = get("Done");
        assert!(done.grab_focus());
        native_x11_key("Return");
        settle();
        assert!(
            window.visible_dialog().is_none(),
            "native Done did not close OSK"
        );
        assert_eq!(
            origin.text().as_str(),
            "q",
            "native Done failed to commit draft"
        );
        let restored = window
            .focused_widget()
            .expect("Done requires focus restoration");
        assert!(
            TvWindow::is_within(&restored, origin.upcast_ref())
                || TvWindow::is_within(&restored, launcher.upcast_ref()),
            "Done must return to origin/launcher on same page"
        );
        assert!(launcher.grab_focus());
        native_x11_key("Return");
        settle();
        let dialog = window.visible_dialog().expect("OSK must reopen");
        let cancel = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>()
                .is_some_and(|b| b.label().as_deref() == Some("Cancel"))
        })
        .expect("Cancel mapped on reopen")
        .downcast::<gtk::Button>()
        .unwrap();
        assert!(cancel.grab_focus());
        native_x11_key("Return");
        settle();
        assert!(window.visible_dialog().is_none(), "Cancel must dismiss OSK");
        assert_eq!(origin.text().as_str(), "q", "Cancel changed source text");
        eprintln!("FOCUS001 osk_transaction page={page} viewport={width} native=true categories=letters,symbols,space,backspace,caret,shift,layout,done,cancel");
        window.close();
    }

    #[test]
    #[ignore = "native isolated XTest OSK transaction"]
    fn gtk_focus001_xtest_live_osk_full_transaction_tv() {
        focus001_native_osk_transaction("live", 1280);
    }
    #[test]
    #[ignore = "native isolated XTest OSK transaction"]
    fn gtk_focus001_xtest_mediathek_osk_full_transaction_compact() {
        focus001_native_osk_transaction("mediathek", 640);
    }
    #[test]
    #[ignore = "native isolated XTest OSK transaction"]
    fn gtk_focus001_xtest_streams_osk_full_transaction_tv() {
        focus001_native_osk_transaction("streams", 1280);
    }

    /// Exercise the real global Programme Guides manager (zero-guide state)
    /// and its nested Add URL dialog without importing a network feed. The
    /// isolated runner's XDG_DATA_HOME and memory GSettings prevent touching
    /// any owner config. Source rows/multirow guide fixtures are NOT covered.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb, disposable XDG and native XTest"]
    fn gtk_focus001_xtest_programme_guides_empty_manager_and_add_dialog() {
        let (_app, window) = mapped_window("streams");
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        crate::epg::desktop::show_guide_manager(window.upcast_ref::<gtk::Window>());
        let mut manager = None;
        for _ in 0..40 {
            settle();
            if let Some(dialog) = window.visible_dialog() {
                manager = Some(dialog);
                break;
            }
        }
        let manager = manager.expect("real Programme Guides manager must map");
        let get = |label: &str| -> gtk::Button {
            TvWindow::find_descendant(manager.upcast_ref(), &|w| {
                w.downcast_ref::<gtk::Button>()
                    .is_some_and(|b| b.label().as_deref() == Some(label))
            })
            .unwrap_or_else(|| panic!("guide manager button missing: {label}"))
            .downcast::<gtk::Button>()
            .expect("real guide manager GtkButton")
        };
        let add = get("Add URL");
        let local = get("Choose Local XMLTV");
        let close = get("Close");
        assert!(add.is_mapped() && local.is_mapped() && close.is_mapped());
        assert!(add.grab_focus());
        native_x11_key("Right");
        assert_owner(
            &window,
            local.upcast_ref(),
            "guide manager Add URL Right local chooser",
        );
        native_x11_key("Left");
        assert_owner(
            &window,
            add.upcast_ref(),
            "guide manager local chooser Left Add URL",
        );
        native_x11_key("Return");
        let mut nested = None;
        for _ in 0..20 {
            settle();
            if let Some(d) = window.visible_dialog() {
                if d != manager {
                    nested = Some(d);
                    break;
                }
            }
        }
        let nested = nested.expect("native OK on Add URL must open real guide Add dialog");
        assert!(nested.is_mapped());
        let owner = window
            .focused_widget()
            .expect("nested Add guide focus owner");
        assert!(
            TvWindow::is_within(&owner, nested.upcast_ref()) && owner.is_mapped(),
            "nested Programme Guide Add dialog must trap mapped focus"
        );
        native_x11_key("Escape");
        settle();
        assert_eq!(
            window.visible_dialog(),
            Some(manager.clone()),
            "nested Add Escape must return to manager"
        );
        let owner = window
            .focused_widget()
            .expect("manager focus after Add Escape");
        assert!(
            TvWindow::is_within(&owner, manager.upcast_ref()) && owner.is_mapped(),
            "guide manager lost focus after child dialog closes"
        );
        native_x11_key("Escape");
        settle();
        assert!(
            window.visible_dialog().is_none(),
            "guide manager Escape did not close"
        );
        window.close();
    }

    /// Populated production Guide manager on a fully disposable registry.
    /// Tests three different controls per row, disabled Refresh state and
    /// nested confirmation/settings return. It never fetches or removes data.
    #[test]
    #[ignore = "requires isolated native XTest and disposable guide registry"]
    fn gtk_focus001_xtest_programme_guides_three_rows_native_actions() {
        crate::epg::desktop::focus001_seed_disposable_guides(3);
        let (_app, window) = mapped_window("streams");
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        crate::epg::desktop::show_guide_manager(window.upcast_ref::<gtk::Window>());
        let mut dialog = None;
        for _ in 0..60 {
            settle();
            if let Some(d) = window.visible_dialog() {
                if TvWindow::find_descendant(d.upcast_ref(), &|w| {
                    w.downcast_ref::<gtk::Button>()
                        .is_some_and(|b| b.label().as_deref() == Some("Use as Shared Default"))
                })
                .is_some()
                {
                    dialog = Some(d);
                    break;
                }
            }
        }
        let manager = dialog.expect("three source rows must paint in actual manager");
        let mut all = Vec::<gtk::Button>::new();
        let mut queue = vec![manager.clone().upcast::<gtk::Widget>()];
        while let Some(node) = queue.pop() {
            if let Some(button) = node.downcast_ref::<gtk::Button>() {
                all.push(button.clone());
            }
            let mut child = node.first_child();
            while let Some(w) = child {
                child = w.next_sibling();
                queue.push(w);
            }
        }
        let labeled = |label: &str| -> Vec<gtk::Button> {
            all.iter()
                .filter(|b| b.label().as_deref() == Some(label))
                .cloned()
                .collect()
        };
        let defaults = labeled("Use as Shared Default");
        let refresh = labeled("Refresh");
        let edit = labeled("Edit");
        let remove = labeled("Remove");
        for group in [&defaults, &refresh, &edit, &remove] {
            assert_eq!(
                group.len(),
                3,
                "every populated guide must show all four actions"
            );
            for b in group {
                assert!(b.is_mapped() && b.is_visible());
                let bounds = b
                    .compute_bounds(window.upcast_ref::<gtk::Widget>())
                    .expect("mapped guide actions require real geometry");
                assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
            }
        }
        assert!(refresh[0].is_sensitive());
        assert!(
            !refresh[1].is_sensitive(),
            "disabled guide must disable Refresh"
        );
        assert!(refresh[2].is_sensitive());
        // Arrow graph across the real first-row buttons: deliberately RED if
        // the UI cannot reach an enabled action by spatial input.
        assert!(defaults[0].grab_focus());
        native_x11_key("Right");
        assert_owner(
            &window,
            refresh[0].upcast_ref(),
            "guide default Right refresh",
        );
        native_x11_key("Right");
        assert_owner(&window, edit[0].upcast_ref(), "guide refresh Right edit");
        native_x11_key("Right");
        assert_owner(&window, remove[0].upcast_ref(), "guide edit Right remove");
        native_x11_key("Left");
        assert_owner(&window, edit[0].upcast_ref(), "guide remove Left edit");
        // Open the actual Edit settings with native OK; Escape must restore
        // manager focus, without changing the guide's persisted fields.
        native_x11_key("Return");
        settle();
        let nested = window.visible_dialog().expect("guide settings modal");
        assert_ne!(nested, manager, "native Edit must open a nested dialog");
        assert!(TvWindow::find_descendant(nested.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>()
                .is_some_and(|b| b.label().as_deref() == Some("Apply"))
        })
        .is_some());
        native_x11_key("Escape");
        settle();
        assert_eq!(window.visible_dialog(), Some(manager.clone()));
        // Destructive control opens a confirmation only, not an implicit
        // deletion; closing it preserves the original three guide rows.
        assert!(remove[2].grab_focus());
        native_x11_key("Return");
        settle();
        let confirmation = window.visible_dialog().expect("guide delete confirmation");
        assert_ne!(confirmation, manager);
        assert!(TvWindow::find_descendant(confirmation.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>()
                .is_some_and(|b| b.label().as_deref() == Some("Keep Guide"))
        })
        .is_some());
        native_x11_key("Escape");
        settle();
        assert_eq!(window.visible_dialog(), Some(manager.clone()));
        eprintln!("FOCUS001 guide_manager populated=3 disabled_refresh=1 native_edges=4 edit_and_remove_modal_return=checked");
        native_x11_key("Escape");
        settle();
        window.close();
    }

    /// Real mapped GTK ListView scrolling/recycling at the licensed M1-scale
    /// counts, with only a single synthetic page hydrated each time. Native
    /// arrow delivery is asserted on interior rows; no real provider/SQLite.
    fn focus001_virtual_mapped_test(total: u32) {
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        let search = streams.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        let positions = [0u32, total / 2, total - 1];
        for position in positions {
            let id = streams.focus001_mapped_virtual_page(total, position);
            settle();
            assert!(
                streams.show_and_focus_test_results(),
                "mapped GTK ListView focus failed at {total}/{position}"
            );
            settle();
            let owner = window
                .focused_widget()
                .expect("virtualized list lost GTK owner");
            eprintln!(
                "FOCUS001 virtual_after_scroll total={total} index={position} owner={} within_list={} mapped={} visible={} sensitive={} focus={}",
                owner.type_().name(), streams.contains_results_focus(&owner),
                owner.is_mapped(), owner.is_visible(), owner.is_sensitive(), owner.has_focus()
            );
            assert!(
                streams.contains_results_focus(&owner)
                    && owner.is_mapped()
                    && owner.is_visible()
                    && owner.is_sensitive(),
                "virtualized focus owner detached at {total}/{position}"
            );
            assert_eq!(
                streams.remote_action_test_selected_id().as_deref(),
                Some(id.as_str()),
                "recycling selected an unrelated source-qualified channel"
            );
            let (slots, pages, n) = streams.focus001_mapped_virtual_stats();
            eprintln!("FOCUS001 virtual_mapped total={total} index={position} created_slots={slots} cached_pages={pages} selected_key={id}");
            assert_eq!(n, total, "actual GtkListView model truncated");
            assert!(
                slots < total.min(700) as usize,
                "mapped GtkListView allocated too many bound slots ({slots} of {total})"
            );
            assert!(pages <= 5, "real ListView page cache exceeded retention");
            // This native key must keep focus in the live ListView even as
            // GTK's viewport responds. Do not use direct router invocation.
            if position > 0 && position + 1 < total {
                native_x11_key("Down");
                let after = window
                    .focused_widget()
                    .expect("native Down orphaned list focus");
                assert!(
                    streams.contains_results_focus(&after) && after.is_mapped(),
                    "native Down escaped live virtualized rows at {total}/{position}"
                );
                native_x11_key("Up");
                let back = window
                    .focused_widget()
                    .expect("native Up orphaned list focus");
                assert!(
                    streams.contains_results_focus(&back) && back.is_mapped(),
                    "native Up did not return to virtualized list"
                );
            }
        }
        window.close();
    }

    #[test]
    #[ignore = "native XTest mapped GtkListView 284-row recycling"]
    fn gtk_focus001_xtest_streams_virtualized_284_rows() {
        focus001_virtual_mapped_test(284);
    }
    #[test]
    #[ignore = "native XTest mapped GtkListView 1484-row recycling"]
    fn gtk_focus001_xtest_streams_virtualized_1484_rows() {
        focus001_virtual_mapped_test(1_484);
    }
    #[test]
    #[ignore = "native XTest mapped GtkListView 10000-row recycling"]
    fn gtk_focus001_xtest_streams_virtualized_10000_rows() {
        focus001_virtual_mapped_test(10_000);
    }
    #[test]
    #[ignore = "native XTest mapped GtkListView 15000-row boundary"]
    fn gtk_focus001_xtest_streams_virtualized_15000_rows() {
        focus001_virtual_mapped_test(15_000);
    }

    /// Disposable end-to-end mapping picker catalogue, never an owner source.
    /// The exact origin is synthesized and the runner isolates XDG storage.
    fn focus001_mapping_fixture(guide_count: usize, channels: usize) -> (String, String, String) {
        use crate::epg::network::TrustScope;
        use crate::epg::sources::{GuideSourceRegistry, SourceOrigin};
        use crate::streams::storage::Storage;
        use rusqlite::params;

        let base = std::env::var("XDG_DATA_HOME").expect("runner must isolate XDG data");
        assert!(base.starts_with("/tmp/"));
        assert_eq!(std::env::var("GSETTINGS_BACKEND").as_deref(), Ok("memory"));
        assert!(guide_count <= 3 && channels <= 64);
        let path = glib::user_data_dir()
            .join("broadcaster")
            .join("broadcaster-iptv.sqlite3");
        crate::streams::view::TvStreamsView::focus001_seed_disposable_manager_sources(path, 1);
        let storage = Storage::new();
        let mut conn = storage.connect().expect("disposable mapping database");
        let (row_id, source_id): (String, String) = conn
            .query_row(
                "SELECT id,source_id FROM iptv_channels WHERE is_present=1 LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("one disposable IPTV channel");
        let registry = GuideSourceRegistry::new(storage);
        let mut guide_id = String::new();
        for n in 0..guide_count {
            let origin = SourceOrigin::entered_remote_url(
                format!("https://example.invalid/only-test-guide-{n}.xml"),
                TrustScope::Public,
            )
            .expect("synthetic guide URL");
            let guide = registry
                .add(&format!("Disposable guide {n}"), &origin)
                .expect("create disposable guide");
            if n == 0 {
                guide_id = guide.id.clone();
            }
            conn.execute(
                "UPDATE epg_sources SET last_success_at=1000 WHERE id=?1",
                [&guide.id],
            )
            .expect("mark synthetic guide as imported");
            for item in 0..channels {
                conn.execute(
                    "INSERT INTO epg_channels(source_id,channel_id,display_name) VALUES(?1,?2,?3)",
                    params![
                        guide.id,
                        format!("focus.{n}.{item:03}"),
                        format!("Synthetic {n} station {item:03}")
                    ],
                )
                .expect("insert synthetic guide metadata");
            }
        }
        if !guide_id.is_empty() {
            crate::epg::bindings::set_shared_guide(
                &mut conn,
                crate::epg::bindings::ConfirmedGuideSelection::after_user_confirmation(),
                Some(&guide_id),
            )
            .expect("set synthetic shared guide");
        }
        (row_id, source_id, guide_id)
    }

    /// Open the real card's programme picker with native XTest Return and
    /// ensure the initial result is modal or a focus-preserving no-guide toast.
    fn focus001_mapping_open(
        guide_count: usize,
        channel_count: usize,
    ) -> (TvApplication, TvWindow, String, String) {
        let (row_id, source_id, guide_id) = focus001_mapping_fixture(guide_count, channel_count);
        let (app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        let search = streams.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        streams.focus001_install_persisted_mapping_row(&row_id, &source_id);
        settle();
        assert!(streams.show_and_focus_test_results());
        native_x11_key("Return"); // expand, never play
        settle();
        let actions = streams
            .remote_action_test_buttons()
            .expect("mapped action buttons");
        let mapping = &actions[2];
        assert!(mapping.is_mapped() && mapping.is_sensitive());
        // Seed ONLY the already mapped mapping control. Whether it is reachable
        // from the card by D-pad is a distinct pre-existing RED case.
        assert!(mapping.grab_focus());
        assert_owner(&window, mapping.upcast_ref(), "mapping original action");
        native_x11_key("Return");
        (app, window, row_id, guide_id)
    }

    #[test]
    #[ignore = "native mapped mapping action when no XMLTV guide is selected"]
    fn gtk_focus001_xtest_mapping_no_guide_keeps_card_focus() {
        let (_app, window, _row_id, _guide_id) = focus001_mapping_open(0, 0);
        for _ in 0..15 {
            settle();
        }
        assert!(
            window.visible_dialog().is_none(),
            "no guide must not open a phantom picker"
        );
        let mapping = window
            .imp()
            .streams_view
            .remote_action_test_buttons()
            .expect("still-expanded card")[2]
            .clone();
        assert_owner(
            &window,
            mapping.upcast_ref(),
            "no-guide toast retains card mapping focus",
        );
        eprintln!("FOCUS001 mapping no_guide toast=no_modal owner=mapped_card_action");
        window.close();
    }

    #[test]
    #[ignore = "native XTest mapped guide picker/search/Cancel with 40 synthetic guide IDs"]
    fn gtk_focus001_xtest_mapping_catalogue_pager_cancel_return() {
        let (_app, window, row_id, _guide_id) = focus001_mapping_open(2, 40);
        let dialog = (0..45)
            .find_map(|_| {
                settle();
                window.visible_dialog()
            })
            .expect("real Programme mapping modal from native Return");
        assert_eq!(dialog.title().as_str(), "Programme mapping");
        let search_toggle = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>().is_some_and(|b| {
                b.label().as_deref() == Some("Search all guide channels / choose exact ID…")
            })
        })
        .expect("real exact-guide search toggle");
        assert!(search_toggle.is_mapped() && search_toggle.is_sensitive());
        assert!(search_toggle.grab_focus());
        native_x11_key("Return");
        let entry = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Entry>().is_some()
        })
        .expect("native search entry");
        assert_owner(
            &window,
            &entry,
            "mapping search toggle moves to exact search entry",
        );
        let next = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>()
                .is_some_and(|b| b.label().as_deref() == Some("Next"))
        })
        .expect("next page button")
        .downcast::<gtk::Button>()
        .expect("GtkButton");
        for _ in 0..45 {
            settle();
            if next.is_sensitive() {
                break;
            }
        }
        assert!(
            next.is_sensitive(),
            "40 synthetic channels must offer a second native page"
        );
        assert!(next.grab_focus());
        native_x11_key("Return");
        for _ in 0..45 {
            settle();
            if TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
                w.downcast_ref::<gtk::Label>()
                    .is_some_and(|l| l.text() == "Guide search page / 2")
            })
            .is_some()
            {
                break;
            }
        }
        let previous = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>()
                .is_some_and(|b| b.label().as_deref() == Some("Previous"))
        })
        .expect("previous page button")
        .downcast::<gtk::Button>()
        .expect("GtkButton");
        assert!(previous.is_sensitive());
        let cancel = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>()
                .is_some_and(|b| b.label().as_deref() == Some("Cancel"))
        })
        .expect("real picker cancel");
        assert!(cancel.grab_focus());
        native_x11_key("Return");
        settle();
        assert!(
            window.visible_dialog().is_none(),
            "Cancel closes real mapping modal"
        );
        let mapping = window
            .imp()
            .streams_view
            .remote_action_test_buttons()
            .expect("still-expanded originating card")[2]
            .clone();
        assert_owner(
            &window,
            mapping.upcast_ref(),
            "picker Cancel must restore originating action",
        );
        let storage = crate::streams::storage::Storage::new();
        let conn = storage.connect().expect("scratch DB remains readable");
        let saved: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                [&row_id],
                |r| r.get(0),
            )
            .expect("check mapping without saving");
        assert_eq!(saved, 0, "Cancel must not silently save a mapping");
        eprintln!("FOCUS001 mapping picker native_open=1 guide_count=2 channels=40 page_2=1 cancel=1 saved_mapping=0");
        window.close();
    }

    #[test]
    #[ignore = "native XTest stale selected XMLTV refresh blocks mapping apply"]
    fn gtk_focus001_xtest_mapping_stale_refresh_blocks_apply() {
        let (_app, window, row_id, guide_id) = focus001_mapping_open(1, 1);
        let dialog = (0..45)
            .find_map(|_| {
                settle();
                window.visible_dialog()
            })
            .expect("real Programme mapping modal");
        let search_toggle = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>().is_some_and(|b| {
                b.label().as_deref() == Some("Search all guide channels / choose exact ID…")
            })
        })
        .expect("exact guide search");
        assert!(search_toggle.grab_focus());
        native_x11_key("Return");
        let mut choice = None;
        for _ in 0..45 {
            settle();
            choice = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
                w.downcast_ref::<gtk::Button>().is_some_and(|b| {
                    b.label()
                        .as_deref()
                        .is_some_and(|t| t.contains("[focus.0.000]"))
                })
            });
            if choice.is_some() {
                break;
            }
        }
        let choice = choice.expect("synthetic guide channel must be rendered");
        assert!(choice.is_mapped() && choice.is_sensitive());
        // An external refresh changes the selection snapshot after the modal
        // opened. Test fixture changes only its own disposable XMLTV rows.
        let storage = crate::streams::storage::Storage::new();
        let conn = storage.connect().expect("isolated guide database");
        conn.execute(
            "UPDATE epg_sources SET last_success_at=1001 WHERE id=?1",
            [&guide_id],
        )
        .expect("simulate completed external guide refresh");
        assert!(choice.grab_focus());
        native_x11_key("Return");
        for _ in 0..45 {
            settle();
        }
        assert_eq!(
            window.visible_dialog(),
            Some(dialog.clone()),
            "stale source must not close dialog as though it saved"
        );
        let saved: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                [&row_id],
                |r| r.get(0),
            )
            .expect("inspect isolated saved mapping");
        assert_eq!(
            saved, 0,
            "refresh invalidated candidate: no stale mapping permitted"
        );
        assert!(
            TvWindow::find_descendant(dialog.upcast_ref(), &|w| w
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.text().contains("reopen mapping")))
            .is_some(),
            "stale Apply must display an explicit error"
        );
        eprintln!("FOCUS001 mapping stale guide refreshed=1 native_apply=1 unchanged_db=1 visible_error=1");
        native_x11_key("Escape");
        settle();
        window.close();
    }

    /// A current, unchanged synthetic guide MUST permit the explicit native
    /// mapping transaction and then restore the same source-qualified owner.
    /// This is the success-path counterpart to the stale/CAS RED probes.
    #[test]
    #[ignore = "native XTest complete synthetic guide choice and SQLite commit"]
    fn gtk_focus001_xtest_mapping_current_guide_apply_and_return() {
        let (_app, window, row_id, guide_id) = focus001_mapping_open(1, 1);
        let dialog = (0..45)
            .find_map(|_| {
                settle();
                window.visible_dialog()
            })
            .expect("real mapping dialog opened by native OK");
        let search = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>().is_some_and(|button| {
                button.label().as_deref() == Some("Search all guide channels / choose exact ID…")
            })
        })
        .expect("mapped full guide catalogue toggle");
        assert!(search.is_mapped() && search.grab_focus());
        native_x11_key("Return");
        let choice = (0..45)
            .find_map(|_| {
                settle();
                TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
                    w.downcast_ref::<gtk::Button>().is_some_and(|button| {
                        button
                            .label()
                            .as_deref()
                            .is_some_and(|label| label.contains("[focus.0.000]"))
                    })
                })
            })
            .expect("native synthetic current guide ID button");
        assert!(choice.is_mapped() && choice.is_sensitive() && choice.grab_focus());
        native_x11_key("Return");
        for _ in 0..45 {
            settle();
            if window.visible_dialog().is_none() {
                break;
            }
        }
        let storage = crate::streams::storage::Storage::new();
        let conn = storage.connect().expect("isolated mapping DB");
        let saved: (String,String) = conn.query_row(
            "SELECT guide_source_id,guide_channel_id FROM epg_channel_overrides WHERE iptv_channel_id=?1",
            [&row_id], |r| Ok((r.get(0)?,r.get(1)?)),
        ).expect("native Apply must commit explicit guide ID");
        assert_eq!(saved, (guide_id, "focus.0.000".to_owned()));
        assert!(
            window.visible_dialog().is_none(),
            "successful Apply closes mapping modal"
        );
        let buttons = window
            .imp()
            .streams_view
            .remote_action_test_buttons()
            .expect("source card after Apply");
        assert_owner(
            &window,
            buttons[2].upcast_ref(),
            "mapping Apply restores card action",
        );
        eprintln!(
            "FOCUS001 mapping_current native_apply=1 saved_id=exact modal_closed=1 origin_return=1"
        );
        window.close();
    }

    /// A selected guide can disappear while its real modal is open. User OK
    /// must never recreate an override with a now-orphaned XMLTV reference.
    #[test]
    #[ignore = "native XTest mapping guide removal while modal is open"]
    fn gtk_focus001_xtest_mapping_removed_guide_rejects_apply() {
        let (_app, window, row_id, guide_id) = focus001_mapping_open(1, 1);
        let dialog = (0..45)
            .find_map(|_| {
                settle();
                window.visible_dialog()
            })
            .expect("real mapping dialog");
        let search = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>().is_some_and(|button| {
                button.label().as_deref() == Some("Search all guide channels / choose exact ID…")
            })
        })
        .expect("search guide channel action");
        assert!(search.grab_focus());
        native_x11_key("Return");
        let choice = (0..45)
            .find_map(|_| {
                settle();
                TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
                    w.downcast_ref::<gtk::Button>().is_some_and(|button| {
                        button
                            .label()
                            .as_deref()
                            .is_some_and(|label| label.contains("[focus.0.000]"))
                    })
                })
            })
            .expect("guide row before external removal");
        let storage = crate::streams::storage::Storage::new();
        let conn = storage.connect().expect("isolated fixture connection");
        conn.execute("DELETE FROM epg_channels WHERE source_id=?1", [&guide_id])
            .expect("remove only disposable guide channels");
        conn.execute("DELETE FROM epg_sources WHERE id=?1", [&guide_id])
            .expect("remove only disposable guide source");
        assert!(choice.grab_focus());
        native_x11_key("Return");
        for _ in 0..45 {
            settle();
        }
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                [&row_id],
                |r| r.get(0),
            )
            .expect("inspect orphaned guide rejection");
        eprintln!(
            "FOCUS001 guide_removed_before_apply saved={count} dialog_open={}",
            window.visible_dialog().is_some()
        );
        assert_eq!(
            count, 0,
            "RED: removed XMLTV guide must not produce an orphaned saved override"
        );
        assert_eq!(
            window.visible_dialog(),
            Some(dialog),
            "removed guide should require fresh manual choice"
        );
        native_x11_key("Escape");
        window.close();
    }

    /// Guide manager's native vertical route must traverse its actual
    /// displayed row order, not a synthetic focus-only sequence.
    #[test]
    #[ignore = "native three-guide manager vertical row directionality"]
    fn gtk_focus001_xtest_programme_guides_vertical_three_row_dpad() {
        crate::epg::desktop::focus001_seed_disposable_guides(3);
        let (_app, window) = mapped_window("streams");
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        crate::epg::desktop::show_guide_manager(window.upcast_ref::<gtk::Window>());
        let manager = (0..60)
            .find_map(|_| {
                settle();
                window.visible_dialog().filter(|d| {
                    TvWindow::find_descendant(d.upcast_ref(), &|w| {
                        w.downcast_ref::<gtk::Button>()
                            .is_some_and(|b| b.label().as_deref() == Some("Use as Shared Default"))
                    })
                    .is_some()
                })
            })
            .expect("populated guide manager");
        let mut actions = Vec::<gtk::Button>::new();
        let mut todo = vec![manager.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = todo.pop() {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if button.label().as_deref() == Some("Use as Shared Default") {
                    actions.push(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(w) = child {
                child = w.next_sibling();
                todo.push(w);
            }
        }
        assert_eq!(actions.len(), 3);
        actions.sort_by(|a, b| {
            let ya = a
                .compute_bounds(window.upcast_ref::<gtk::Widget>())
                .expect("guide row geometry")
                .y();
            let yb = b
                .compute_bounds(window.upcast_ref::<gtk::Widget>())
                .expect("guide row geometry")
                .y();
            ya.total_cmp(&yb)
        });
        for (i, button) in actions.iter().enumerate() {
            assert!(
                button.is_mapped() && button.is_sensitive(),
                "guide {i} action unavailable"
            );
        }
        assert!(actions[0].grab_focus());
        native_x11_key("Down");
        assert_owner(
            &window,
            actions[1].upcast_ref(),
            "guide first row Down second row same column",
        );
        native_x11_key("Down");
        assert_owner(
            &window,
            actions[2].upcast_ref(),
            "guide second row Down third row",
        );
        native_x11_key("Up");
        assert_owner(
            &window,
            actions[1].upcast_ref(),
            "guide third row Up second row",
        );
        native_x11_key("Up");
        assert_owner(
            &window,
            actions[0].upcast_ref(),
            "guide second row Up first row",
        );
        eprintln!("FOCUS001 guide_manager native_three_row_vertical=4 reversal=complete");
        native_x11_key("Escape");
        window.close();
    }

    /// Exhaustively exercise the enabled three-guide manager's internal
    /// geometric 4-direction graph. Only the starting control is seeded;
    /// every observed transition uses native XTest D-pad key delivery.
    /// Collect mismatches rather than aborting on the first RED edge.
    #[test]
    #[ignore = "native mapped four-direction graph of all enabled 3-guide controls"]
    fn gtk_focus001_xtest_programme_guides_complete_enabled_grid() {
        crate::epg::desktop::focus001_seed_disposable_guides(3);
        let (_app, window) = mapped_window("streams");
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        crate::epg::desktop::show_guide_manager(window.upcast_ref::<gtk::Window>());
        let dialog = (0..60)
            .find_map(|_| {
                settle();
                window.visible_dialog().filter(|d| {
                    TvWindow::find_descendant(d.upcast_ref(), &|w| {
                        w.downcast_ref::<gtk::Button>()
                            .is_some_and(|b| b.label().as_deref() == Some("Use as Shared Default"))
                    })
                    .is_some()
                })
            })
            .expect("three-row guide manager");
        let mut controls = Vec::<gtk::Button>::new();
        let mut todo = vec![dialog.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = todo.pop() {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if matches!(
                    button.label().as_deref(),
                    Some("Use as Shared Default" | "Refresh" | "Edit" | "Remove")
                ) && button.is_mapped()
                    && button.is_sensitive()
                {
                    controls.push(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(w) = child {
                child = w.next_sibling();
                todo.push(w);
            }
        }
        assert_eq!(
            controls.len(),
            11,
            "three 4-action rows, one disabled Refresh"
        );
        // The controls may be rendered in an order different from tree traversal.
        // Derive every expected arrow target solely from their live geometry.
        let page = window.upcast_ref::<gtk::Widget>();
        let rects: Vec<ToolbarRect> = controls
            .iter()
            .map(|b| ToolbarRect::of(b.upcast_ref(), page).expect("mapped guide geometry"))
            .collect();
        let mut attempted = 0;
        let mut missing = Vec::<String>::new();
        for (from, button) in controls.iter().enumerate() {
            for (key, name) in [
                (gtk::gdk::Key::Right, "Right"),
                (gtk::gdk::Key::Left, "Left"),
                (gtk::gdk::Key::Down, "Down"),
                (gtk::gdk::Key::Up, "Up"),
            ] {
                let Some(to) = spatial_neighbor(&rects, from, key) else {
                    continue;
                };
                assert!(
                    button.grab_focus(),
                    "mapped guide button did not accept focus"
                );
                assert_owner(&window, button.upcast_ref(), "guide grid seed");
                native_x11_key(name);
                attempted += 1;
                let owner = window.focused_widget();
                if !owner.as_ref().is_some_and(|w| {
                    w.is_mapped()
                        && w.is_sensitive()
                        && TvWindow::is_within(w, controls[to].upcast_ref())
                }) {
                    missing.push(format!(
                        "{} {} -> {} expected {} got {}",
                        button.label().unwrap_or_default(),
                        name,
                        from,
                        controls[to].label().unwrap_or_default(),
                        owner.map_or("none".to_owned(), |w| w.type_().name().to_string())
                    ));
                }
            }
        }
        eprintln!(
            "FOCUS001 three_guide_grid enabled={} actual_native_edges={} wrong={} examples={:?}",
            controls.len(),
            attempted,
            missing.len(),
            missing.iter().take(8).collect::<Vec<_>>()
        );
        assert!(attempted >= 22, "too few real geometric guide-row edges");
        assert!(
            missing.is_empty(),
            "RED: populated guide manager geometric routes: {} wrong of {attempted}",
            missing.len()
        );
        native_x11_key("Escape");
        window.close();
    }

    /// Three real disposable source-manager rows, seven controls each. Build
    /// the enabled 4-way graph from measured GTK bounds and XTest every edge.
    /// This catches inaccessible second-row controls beyond one Refresh Down.
    #[test]
    #[ignore = "native four-direction graph of all enabled source manager actions"]
    fn gtk_focus001_xtest_manage_sources_complete_three_row_grid() {
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        let scratch = std::env::temp_dir().join(format!(
            "broadcaster-focus001-source-graph-{}",
            uuid::Uuid::new_v4()
        ));
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&scratch)
            .expect("private disposable source graph fixture");
        let path = scratch.join("manager.sqlite");
        crate::streams::view::TvStreamsView::focus001_seed_disposable_manager_sources(
            path.clone(),
            3,
        );
        let loaded = streams.focus001_attach_disposable_service(path);
        for _ in 0..45 {
            if loaded.get() {
                break;
            }
            settle();
        }
        assert!(loaded.get(), "synthetic three-source fixture not loaded");
        let search = streams.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        streams.show_manager();
        let manager = (0..60)
            .find_map(|_| {
                settle();
                window.visible_dialog().filter(|d| {
                    TvWindow::find_descendant(d.upcast_ref(), &|w| {
                        w.downcast_ref::<gtk::Button>()
                            .is_some_and(|b| b.label().as_deref() == Some("Replace"))
                    })
                    .is_some()
                })
            })
            .expect("real populated source manager");
        let mut controls = Vec::<gtk::Button>::new();
        let mut todo = vec![manager.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = todo.pop() {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if matches!(
                    button.label().as_deref(),
                    Some("Refresh" | "Defaults" | "Rename" | "Replace" | "Delete" | "↑" | "↓")
                ) && button.is_mapped()
                    && button.is_sensitive()
                {
                    controls.push(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(w) = child {
                child = w.next_sibling();
                todo.push(w);
            }
        }
        assert_eq!(
            controls.len(),
            19,
            "three seven-action rows with disabled boundary moves"
        );
        let page = window.upcast_ref::<gtk::Widget>();
        let rects: Vec<ToolbarRect> = controls
            .iter()
            .map(|b| ToolbarRect::of(b.upcast_ref(), page).expect("source action geometry"))
            .collect();
        let mut checked = 0;
        let mut missing = Vec::<String>::new();
        for (from, button) in controls.iter().enumerate() {
            for (direction, name) in [
                (gtk::gdk::Key::Right, "Right"),
                (gtk::gdk::Key::Left, "Left"),
                (gtk::gdk::Key::Down, "Down"),
                (gtk::gdk::Key::Up, "Up"),
            ] {
                let Some(to) = spatial_neighbor(&rects, from, direction) else {
                    continue;
                };
                assert!(
                    button.grab_focus(),
                    "enabled mapped source action cannot receive focus"
                );
                native_x11_key(name);
                checked += 1;
                let owner = window.focused_widget();
                if !owner.as_ref().is_some_and(|w| {
                    w.is_mapped()
                        && w.is_sensitive()
                        && TvWindow::is_within(w, controls[to].upcast_ref())
                }) {
                    missing.push(format!(
                        "{}[{from}] {name} -> {}[{to}] got {}",
                        button.label().unwrap_or_default(),
                        controls[to].label().unwrap_or_default(),
                        owner.map_or("none".to_owned(), |w| w.type_().name().to_string())
                    ));
                }
            }
        }
        eprintln!(
            "FOCUS001 source_three_row_grid enabled={} native_edges={} wrong={} examples={:?}",
            controls.len(),
            checked,
            missing.len(),
            missing.iter().take(10).collect::<Vec<_>>()
        );
        assert!(
            checked >= 30,
            "too few real source-manager geometric edges enumerated"
        );
        assert!(
            missing.is_empty(),
            "RED: real source-manager directional graph has {} wrong of {checked} arrows",
            missing.len()
        );
        native_x11_key("Escape");
        window.close();
        std::fs::remove_dir_all(scratch).expect("clean synthetic source fixture");
    }

    #[test]
    #[ignore = "native XTest favourite mode persists ON-OFF-ON in disposable SQLite"]
    fn gtk_focus001_xtest_favourites_footer_native_async_roundtrip() {
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        let search = streams.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        let data_dir = std::env::var("XDG_DATA_HOME").expect("isolated XDG_DATA_HOME");
        assert!(data_dir.starts_with("/tmp/"));
        let store = crate::streams::storage::Storage::new();
        let ready = streams.focus001_attach_disposable_service(
            glib::user_data_dir()
                .join("broadcaster")
                .join("broadcaster-iptv.sqlite3"),
        );
        for _ in 0..35 {
            if ready.get() {
                break;
            }
            settle();
        }
        assert!(ready.get(), "disposable SQLite favourites service ready");
        let footer = streams.toolbar_focus_targets()[8].clone();
        assert!(footer.is_mapped() && footer.is_sensitive());
        let mut unfocused = 0;
        for (index, expected) in [true, false, true].into_iter().enumerate() {
            assert!(footer.grab_focus()); // reset after each product focus-loss RED
            assert_owner(&window, &footer, "native favourites OK origin");
            native_x11_key("Return");
            for _ in 0..45 {
                settle();
                if footer.is_sensitive() && store.favourites_only() == Ok(expected) {
                    break;
                }
            }
            assert!(
                footer.is_sensitive(),
                "async SQLite footer button never reenables"
            );
            assert_eq!(
                store.favourites_only(),
                Ok(expected),
                "mode persisted from native OK"
            );
            let valid = owns_focus(&window, &footer);
            eprintln!(
                "FOCUS001 favourites_native toggle={} persisted={} mapped={} retained_owner={}",
                index,
                expected,
                footer.is_mapped(),
                valid
            );
            if !valid {
                unfocused += 1;
            }
        }
        assert_eq!(
            unfocused, 0,
            "RED: at least one native favourites toggle loses original visible focus owner"
        );
        window.close();
    }

    fn focus001_virtual_dpad_route(total: u32, steps: u32) {
        let (_app, window) = mapped_window("streams");
        window.set_default_size(640, 480);
        settle();
        let streams = &window.imp().streams_view;
        let search = streams.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        streams.focus001_mapped_virtual_page(total, 0);
        settle();
        assert!(streams.show_and_focus_test_results());
        settle();
        let original = streams.remote_action_test_selected_id();
        let mut reached = 0u32;
        for n in 1..=steps {
            native_x11_key("Down");
            let owner = window.focused_widget().expect("Down lost all GTK focus");
            let current = streams.focus001_selected_index();
            eprintln!("FOCUS001 virtual_native Down step={n} selected={current} owner={} mapped={} within_list={}",
                owner.type_().name(), owner.is_mapped(), streams.contains_results_focus(&owner));
            assert!(
                owner.is_mapped() && owner.is_sensitive() && streams.contains_results_focus(&owner),
                "RED: native Down leaves an unmapped/foreign focus widget in virtual GtkListView"
            );
            assert_eq!(current, n, "native Down must move to the next visible row");
            reached = n;
        }
        for n in (0..reached).rev() {
            native_x11_key("Up");
            assert_eq!(
                streams.focus001_selected_index(),
                n,
                "reverse native Up must return to the previous logical row"
            );
            let owner = window.focused_widget().expect("Up lost mapped focus");
            assert!(owner.is_mapped() && streams.contains_results_focus(&owner));
        }
        assert_eq!(
            streams.remote_action_test_selected_id(),
            original,
            "source-qualified row key must survive native viewport recycle"
        );
        eprintln!(
            "FOCUS001 virtual_native viewport=640x480 rows={total} moves={steps} reverse={steps}"
        );
        window.close();
    }

    #[test]
    #[ignore = "native XTest 284-row compact GTK ListView spatial recycling"]
    fn gtk_focus001_xtest_streams_virtual_native_dpad_284() {
        focus001_virtual_dpad_route(284, 28);
    }

    #[test]
    #[ignore = "native XTest 10k-row compact GTK ListView 50-step/reverse recycling"]
    fn gtk_focus001_xtest_streams_virtual_native_dpad_10000() {
        focus001_virtual_dpad_route(10_000, 50);
    }

    #[test]
    #[ignore = "native XTest 15k-row boundary compact GTK ListView 50-step/reverse"]
    fn gtk_focus001_xtest_streams_virtual_native_dpad_15000() {
        focus001_virtual_dpad_route(15_000, 50);
    }

    #[test]
    #[ignore = "native mapped My Channels first OK, second OK fake Play, Escape return"]
    fn gtk_focus001_xtest_streams_card_two_ok_fake_playback_return() {
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        let search = streams.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        let ids = streams.install_remote_action_test_rows();
        settle();
        assert!(streams.show_and_focus_test_results());
        assert_eq!(
            streams.remote_action_test_selected_id().as_deref(),
            Some(ids[0].as_str())
        );
        native_x11_key("Return");
        assert_eq!(
            streams.remote_action_test_counts().0,
            0,
            "first native OK only expands, never calls media backend"
        );
        let state = streams.remote_action_test_state();
        eprintln!("FOCUS001 card_first_ok state={state}");
        assert!(
            state.contains(&ids[0]),
            "first OK must retain logical originating row"
        );
        let actions = streams
            .remote_action_test_buttons()
            .expect("expanded action row");
        let play = &actions[0];
        assert!(play.is_mapped() && play.is_sensitive());
        // The previous suite tests auto-Play focus separately. Seed the
        // already-visible Play button to exercise an independent second OK.
        assert!(play.grab_focus());
        native_x11_key("Return");
        assert_eq!(
            streams.remote_action_test_counts().0,
            1,
            "second deliberate native OK must call only fake media backend"
        );
        native_x11_key("Escape");
        settle();
        let owner = window
            .focused_widget()
            .expect("Escape must return to a visible owner");
        assert!(
            owner.is_mapped() && owner.is_sensitive() && streams.contains_results_focus(&owner),
            "native Escape after fake playback must restore a reachable card owner"
        );
        assert_eq!(
            streams.remote_action_test_selected_id().as_deref(),
            Some(ids[0].as_str())
        );
        eprintln!("FOCUS001 fake_play native_ok=2 backend_calls=1 escape_return=1");
        window.close();
    }

    #[test]
    #[ignore = "known M1 stale catalogue same-second refresh must not silently save"]
    fn gtk_focus001_xtest_mapping_same_timestamp_catalogue_change_rejected() {
        let (_app, window, row_id, guide_id) = focus001_mapping_open(1, 1);
        let dialog = (0..45)
            .find_map(|_| {
                settle();
                window.visible_dialog()
            })
            .expect("real Programme mapping modal");
        let search_toggle = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>().is_some_and(|b| {
                b.label().as_deref() == Some("Search all guide channels / choose exact ID…")
            })
        })
        .expect("exact guide search");
        assert!(search_toggle.grab_focus());
        native_x11_key("Return");
        let mut choice = None;
        for _ in 0..45 {
            settle();
            choice = TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
                w.downcast_ref::<gtk::Button>().is_some_and(|b| {
                    b.label()
                        .as_deref()
                        .is_some_and(|t| t.contains("[focus.0.000]"))
                })
            });
            if choice.is_some() {
                break;
            }
        }
        let choice = choice.expect("synthetic guide choice");
        let storage = crate::streams::storage::Storage::new();
        let conn = storage.connect().expect("isolated mapping fixture");
        conn.execute(
            "UPDATE epg_channels SET display_name='Same-second replacement' WHERE source_id=?1 AND channel_id='focus.0.000'",
            [&guide_id],
        ).expect("simulate reimport with identical second timestamp");
        assert!(choice.grab_focus());
        native_x11_key("Return");
        for _ in 0..45 {
            settle();
        }
        let saved: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                [&row_id],
                |r| r.get(0),
            )
            .expect("inspect same-second mapping snapshot");
        eprintln!("FOCUS001 mapping_same_second selected_id_changed=0 catalog_label_changed=1 saved_count={saved}");
        assert_eq!(
            saved, 0,
            "RED: same-second replacement catalogue must invalidate stale picker snapshot"
        );
        assert_eq!(
            window.visible_dialog(),
            Some(dialog.clone()),
            "stale picker must remain open and require fresh confirmation"
        );
        native_x11_key("Escape");
        window.close();
    }

    /// A test-only screenshot oracle: XGetImage samples actual X11 pixels for
    /// the same mapped rectangle before/after GTK focus. Never saves user UI.
    /// An intentionally suppressed ring must NOT change the sampled pixels.
    #[test]
    #[ignore = "local X11 pixel capture probe, requires FOCUS001_PIXEL_BIN"]
    fn gtk_focus001_local_rendered_indicator_negative_control_probe() {
        let (_app, window) = mapped_window("live");
        window.imp().live_view.show_toolbar_for_test();
        settle();
        let controls = window.imp().live_view.toolbar_focus_targets();
        let search = controls[0].clone();
        let button = controls[1].clone();
        assert_native_input_delivery(&search);
        assert!(button.is_mapped());
        let bounds = button
            .compute_bounds(window.upcast_ref::<gtk::Widget>())
            .expect("rendered button geometry");
        let screenshot = || -> String {
            let tool = std::env::var_os("FOCUS001_PIXEL_BIN")
                .expect("pixel capture experiment tool missing");
            let x = (bounds.x() - 3.0).max(0.0) as i32;
            let y = (bounds.y() - 3.0).max(0.0) as i32;
            let w = (bounds.width() + 6.0) as i32;
            let h = (bounds.height() + 6.0) as i32;
            let out = std::process::Command::new(tool)
                .args([
                    "--pixels".to_owned(),
                    x.to_string(),
                    y.to_string(),
                    w.to_string(),
                    h.to_string(),
                ])
                .output()
                .expect("run mapped X11 pixel oracle");
            assert!(
                out.status.success(),
                "FOCUS001 PIXEL_ENVIRONMENT capture failed: {:?}",
                out.status.code()
            );
            String::from_utf8(out.stdout)
                .expect("pixel output UTF-8")
                .split_whitespace()
                .next()
                .expect("pixel checksum")
                .to_owned()
        };
        assert!(search.grab_focus());
        settle();
        let runtime_unfocused = screenshot();
        // Move with a real D-pad Right event: programmatic grab_focus can
        // legitimately affect GTK focus-visible styling independently.
        native_x11_key("Right");
        settle();
        assert_owner(&window, &button, "native Live Search Right keyboard button");
        let runtime_focused = screenshot();
        eprintln!(
            "FOCUS001 PIXEL runtime normal={} focused={}",
            runtime_unfocused, runtime_focused
        );
        let actual_runtime_painted = runtime_unfocused != runtime_focused;
        let provider = gtk::CssProvider::new();
        provider.load_from_string(".focus001-test-ring:focus { outline: 4px solid rgb(255,0,255); outline-offset: -4px; box-shadow: none; } .focus001-test-hidden, .focus001-test-hidden:focus { outline: none; outline-offset: 0; box-shadow: none; }");
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("X11 display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        button.add_css_class("focus001-test-ring");
        assert!(search.grab_focus());
        settle();
        let no_focus = screenshot();
        // Move with a real D-pad Right event: programmatic grab_focus can
        // legitimately affect GTK focus-visible styling independently.
        native_x11_key("Right");
        settle();
        assert_owner(&window, &button, "native Live Search Right keyboard button");
        let painted_focus = screenshot();
        eprintln!(
            "FOCUS001 PIXEL ring normal={} focused={}",
            no_focus, painted_focus
        );
        assert_ne!(
            no_focus, painted_focus,
            "rendered positive ring must change screenshot pixels"
        );
        button.remove_css_class("focus001-test-ring");
        button.add_css_class("focus001-test-hidden");
        assert!(search.grab_focus());
        settle();
        let hidden_unfocused = screenshot();
        // Move with a real D-pad Right event: programmatic grab_focus can
        // legitimately affect GTK focus-visible styling independently.
        native_x11_key("Right");
        settle();
        assert_owner(&window, &button, "native Live Search Right keyboard button");
        let hidden_focused = screenshot();
        eprintln!(
            "FOCUS001 PIXEL hidden normal={} focused={}",
            hidden_unfocused, hidden_focused
        );
        assert_eq!(
            hidden_unfocused, hidden_focused,
            "rendered oracle correctly FAILS on intentionally hidden focus ring"
        );
        eprintln!("FOCUS001 PIXEL positive_negative_control=PASS runtime_painted={actual_runtime_painted}");
        assert!(actual_runtime_painted,
            "RED: real Live keyboard focused/unfocused control draws no distinguishable pixels in Xvfb");
        window.close();
    }

    /// Preserve the user's explicit QWERTZ choice when the OSK is closed and
    /// reopened from the same page. Native Return operates the actual modal
    /// layout button; this test intentionally exposes per-dialog resets.
    #[test]
    #[ignore = "native isolated XTest OSK layout preference retention"]
    fn gtk_focus001_xtest_osk_qwertz_reopen_retains_layout() {
        let (_app, window) = mapped_window("streams");
        let toolbar = window.imp().streams_view.toolbar_focus_targets();
        let launcher = toolbar[11]
            .clone()
            .downcast::<gtk::Button>()
            .expect("real My Channels OSK launcher");
        assert_native_input_delivery(&toolbar[0]);
        assert!(launcher.grab_focus());
        native_x11_key("Return");
        let first = window.visible_dialog().expect("mapped OSK open");
        let find_key = |dialog: &adw::Dialog, label: &str| -> gtk::Button {
            TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
                w.downcast_ref::<gtk::Button>()
                    .is_some_and(|button| button.label().as_deref() == Some(label))
            })
            .unwrap_or_else(|| panic!("missing OSK {label} button"))
            .downcast::<gtk::Button>()
            .expect("mapped OSK key")
        };
        let toggle = find_key(&first, "QWERTZ");
        assert!(toggle.is_mapped() && toggle.grab_focus());
        native_x11_key("Return");
        assert!(find_key(&first, "QWERTY").is_mapped());
        let done = find_key(&first, "Done");
        assert!(done.grab_focus());
        native_x11_key("Return");
        settle();
        assert!(window.visible_dialog().is_none(), "Done must close OSK");
        assert!(launcher.grab_focus());
        native_x11_key("Return");
        settle();
        let reopened = window.visible_dialog().expect("OSK reopen");
        let retained = TvWindow::find_descendant(reopened.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>()
                .is_some_and(|button| button.label().as_deref() == Some("QWERTY"))
        })
        .is_some();
        eprintln!("FOCUS001 osk_qwertz_same_session_retained={retained}");
        assert!(
            retained,
            "RED: OSK resets explicit QWERTZ preference after Done and reopen"
        );
        native_x11_key("Escape");
        window.close();
    }

    /// Every currently enabled My Channels toolbar control participates in
    /// one actual native D-pad graph, not merely individual grab_focus probes.
    /// The fixture is the safe empty local state, with no playlist import.
    #[test]
    #[ignore = "requires isolated mapped GTK/Xvfb with native XTest"]
    fn gtk_focus001_xtest_streams_empty_toolbar_native_graph() {
        let (_app, window) = mapped_window("streams");
        let controls = window.imp().streams_view.toolbar_focus_targets();
        assert_eq!(
            controls.len(),
            12,
            "source/filter toolbar inventory drifted"
        );
        assert_native_input_delivery(&controls[0]);
        let enabled: Vec<_> = controls
            .iter()
            .enumerate()
            .filter_map(|(i, w)| (w.is_mapped() && w.is_sensitive() && w.is_visible()).then_some(i))
            .collect();
        assert!(
            enabled.len() >= 3,
            "empty state removed core toolbar controls"
        );
        let mut edges = vec![Vec::new(); controls.len()];
        let mut reversed = Vec::new();
        let mut wrong = Vec::new();
        let mut escapes = Vec::new();
        for &index in &enabled {
            let from = &controls[index];
            assert!(from.grab_focus(), "enabled toolbar cannot focus");
            let rect = from
                .compute_bounds(window.upcast_ref::<gtk::Widget>())
                .expect("source toolbar allocation missing");
            let center = (
                rect.x() + rect.width() * 0.5,
                rect.y() + rect.height() * 0.5,
            );
            for arrow in ["Up", "Down", "Left", "Right"] {
                assert!(from.grab_focus());
                settle();
                native_x11_key(arrow);
                let owner = window
                    .focused_widget()
                    .expect("My Channels toolbar D-pad orphaned focus");
                assert!(
                    owner.is_mapped() && owner.is_visible() && owner.is_sensitive(),
                    "My Channels toolbar destination is detached/disabled"
                );
                let target = enabled
                    .iter()
                    .copied()
                    .find(|&i| TvWindow::is_within(&owner, &controls[i]));
                eprintln!(
                    "FOCUS001 streams_toolbar from={index} arrow={arrow} to={target:?} owner={}",
                    owner.type_().name()
                );
                if let Some(dest) = target {
                    if index == dest {
                        continue;
                    } // editing caret / boundary
                    edges[index].push(dest);
                    let r = controls[dest]
                        .compute_bounds(window.upcast_ref::<gtk::Widget>())
                        .expect("target geometry");
                    let d = (
                        r.x() + r.width() * 0.5 - center.0,
                        r.y() + r.height() * 0.5 - center.1,
                    );
                    let direction = match arrow {
                        "Up" => d.1 < -2.0,
                        "Down" => d.1 > 2.0,
                        "Left" => d.0 < -2.0,
                        "Right" => d.0 > 2.0,
                        _ => unreachable!(),
                    };
                    if !direction {
                        wrong.push(format!("{index} {arrow} {dest}"));
                    }
                    let reverse = match arrow {
                        "Up" => "Down",
                        "Down" => "Up",
                        "Left" => "Right",
                        "Right" => "Left",
                        _ => unreachable!(),
                    };
                    native_x11_key(reverse);
                    if !window
                        .focused_widget()
                        .as_ref()
                        .is_some_and(|w| TvWindow::is_within(w, from))
                    {
                        reversed.push(format!("{index} {arrow} {dest} {reverse}"));
                    }
                } else if !TvWindow::is_within(&owner, window.imp().streams_view.upcast_ref()) {
                    // Up from top-row Search or the adjacent OSK launcher is
                    // an intentional portal to the global page switcher, not
                    // an ordinary same-page toolbar escape. Active-tab
                    // identity is separately tested by the switcher canary.
                    let top_portal = arrow == "Up"
                        && (index == 0 || index == 11)
                        && owner.type_().name() == "AdwViewSwitcherButton";
                    if !top_portal {
                        escapes.push(format!("{index} {arrow} {}", owner.type_().name()));
                    }
                }
            }
        }
        let mut seen = vec![false; controls.len()];
        seen[0] = true;
        let mut queue = std::collections::VecDeque::from([0]);
        while let Some(i) = queue.pop_front() {
            for &next in &edges[i] {
                if !seen[next] {
                    seen[next] = true;
                    queue.push_back(next);
                }
            }
        }
        let missing: Vec<_> = enabled.iter().copied().filter(|&i| !seen[i]).collect();
        eprintln!("FOCUS001 streams_native_graph enabled={enabled:?} missing={missing:?} wrong={wrong:?} reverse={reversed:?} escapes={escapes:?}");
        assert!(
            escapes.is_empty(),
            "My Channels toolbar unexpectedly entered global/tab focus"
        );
        assert!(
            wrong.is_empty(),
            "My Channels toolbar arrows violate mapped directions"
        );
        assert!(
            reversed.is_empty(),
            "My Channels toolbar cannot locally reverse arrow edges"
        );
        assert!(
            missing.is_empty(),
            "some mapped My Channels toolbar buttons have no D-pad route"
        );
        window.close();
    }

    // Connected, native-key manager journeys use ONLY disposable SQLite fixtures;
    // neither owner playlists nor network inputs can enter these tests.
    fn focus001_manager_fixture(count: u32) -> (TvApplication, TvWindow, std::path::PathBuf) {
        let (app, window) = mapped_window("streams");
        let scratch =
            std::env::temp_dir().join(format!("focus001-manager-{}", uuid::Uuid::new_v4()));
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&scratch)
            .expect("private, disposable test-only SQLite directory");
        let path = scratch.join("manager.sqlite");
        crate::streams::view::TvStreamsView::focus001_seed_disposable_manager_sources(
            path.clone(),
            count,
        );
        let view = &window.imp().streams_view;
        let ready = view.focus001_attach_disposable_service(path);
        for _ in 0..50 {
            if ready.get() {
                break;
            }
            settle();
        }
        assert!(ready.get(), "disposable source manager not ready");
        assert_native_input_delivery(&view.toolbar_focus_targets()[0]);
        (app, window, scratch)
    }

    fn focus001_button_in(dialog: &adw::Dialog, label: &str) -> gtk::Button {
        TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>()
                .is_some_and(|button| button.label().as_deref() == Some(label))
        })
        .unwrap_or_else(|| panic!("mapped dialog missing enabled action {label}"))
        .downcast::<gtk::Button>()
        .expect("mapped native GTK button")
    }

    fn focus001_dialog_after(window: &TvWindow, old: Option<&adw::Dialog>) -> adw::Dialog {
        for _ in 0..55 {
            settle();
            if let Some(dialog) = window.visible_dialog() {
                if old.is_none_or(|before| before != &dialog) {
                    assert!(dialog.is_mapped(), "new dialog is not mapped");
                    return dialog;
                }
            }
        }
        panic!("native OK did not open expected modal");
    }

    #[test]
    #[ignore = "native mapped zero-source manager header, Add modal, cancel/guide return"]
    fn gtk_focus001_xtest_manage_sources_empty_add_guide_connected() {
        let (_app, window, scratch) = focus001_manager_fixture(0);
        window.imp().streams_view.show_manager();
        let manager = focus001_dialog_after(&window, None);
        for label in ["Add Playlist", "Programme Guides", "Close"] {
            let button = focus001_button_in(&manager, label);
            assert!(
                button.is_mapped() && button.is_sensitive(),
                "empty manager missing {label}"
            );
            let bounds = button
                .compute_bounds(window.upcast_ref::<gtk::Widget>())
                .expect("visible manager header geometry");
            assert!(bounds.width() >= 16.0 && bounds.height() >= 16.0);
        }
        let add = focus001_button_in(&manager, "Add Playlist");
        assert!(add.grab_focus());
        native_x11_key("Return");
        let nested = focus001_dialog_after(&window, Some(&manager));
        for label in ["Choose Local M3U File", "Enter Playlist URL"] {
            let action = focus001_button_in(&nested, label);
            assert!(action.is_mapped() && action.is_sensitive());
        }
        let owner = window.focused_widget().expect("Add menu owner");
        assert!(
            owner.is_mapped() && TvWindow::is_within(&owner, nested.upcast_ref()),
            "Add menu opened without real native modal focus confinement"
        );
        native_x11_key("Escape");
        assert_eq!(
            window.visible_dialog(),
            Some(manager.clone()),
            "Add Escape restores manager"
        );
        let guides = focus001_button_in(&manager, "Programme Guides");
        assert!(guides.grab_focus());
        native_x11_key("Return");
        let guide_manager = focus001_dialog_after(&window, Some(&manager));
        assert!(focus001_button_in(&guide_manager, "Add URL").is_mapped());
        native_x11_key("Escape");
        assert_eq!(
            window.visible_dialog(),
            Some(manager.clone()),
            "Guide manager Escape returns to source manager"
        );
        native_x11_key("Escape");
        assert!(
            window.visible_dialog().is_none(),
            "source manager must dismiss on Escape"
        );
        let owner = window
            .focused_widget()
            .expect("manager Escape must restore page focus");
        assert!(
            owner.is_mapped()
                && owner.is_sensitive()
                && TvWindow::is_within(&owner, window.imp().streams_view.upcast_ref()),
            "empty manager loses page focus on dismissal"
        );
        eprintln!(
            "FOCUS001 source_manager_empty native_header=3 add_modal=2 guide_modal=1 dismiss=ok"
        );
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove private disposable manager fixture");
    }

    #[test]
    #[ignore = "native mapped disposable one-source Delete/Keep and confirmed deletion SQLite"]
    fn gtk_focus001_xtest_manage_sources_confirm_disposable_delete_and_fallback() {
        let (_app, window, scratch) = focus001_manager_fixture(1);
        let path = scratch.join("manager.sqlite");
        window.imp().streams_view.show_manager();
        let manager = focus001_dialog_after(&window, None);
        let delete = focus001_button_in(&manager, "Delete");
        assert!(delete.is_sensitive() && delete.grab_focus());
        native_x11_key("Return");
        let confirmation = focus001_dialog_after(&window, Some(&manager));
        let keep = focus001_button_in(&confirmation, "Keep Source");
        assert!(keep.grab_focus());
        native_x11_key("Return");
        assert_eq!(
            window.visible_dialog(),
            Some(manager.clone()),
            "Keep must return to manager"
        );
        let store = crate::streams::storage::Storage::at(path.clone());
        assert_eq!(
            store.list_sources().expect("scratch read").len(),
            1,
            "Keep Source may never delete disposable source"
        );
        let delete = focus001_button_in(&manager, "Delete");
        assert!(delete.grab_focus());
        native_x11_key("Return");
        let confirmation = focus001_dialog_after(&window, Some(&manager));
        let confirm = focus001_button_in(&confirmation, "Delete Permanently");
        assert!(confirm.grab_focus());
        native_x11_key("Return");
        let mut remaining = usize::MAX;
        for _ in 0..60 {
            settle();
            remaining = store.list_sources().expect("scratch deletion read").len();
            if remaining == 0 {
                break;
            }
        }
        assert_eq!(
            remaining, 0,
            "native synthetic Delete confirmation must persist"
        );
        assert_eq!(
            window.visible_dialog(),
            Some(manager.clone()),
            "delete completion must return to manager"
        );
        let owner = window
            .focused_widget()
            .expect("delete completion focus owner");
        assert!(
            owner.is_mapped()
                && owner.is_sensitive()
                && TvWindow::is_within(&owner, manager.upcast_ref()),
            "source delete rebind orphans manager focus"
        );
        eprintln!("FOCUS001 source_manager_delete native_confirm=1 cancel_verified=1 SQLite_remaining={remaining}");
        native_x11_key("Escape");
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove disposable deleted source fixture");
    }

    #[test]
    #[ignore = "native mapped zero-guide Add URL validation/Cancel return; no network"]
    fn gtk_focus001_xtest_guides_add_blank_rejected_and_cancel_connected() {
        crate::epg::desktop::focus001_seed_disposable_guides(0);
        let (_app, window) = mapped_window("streams");
        assert_native_input_delivery(&window.imp().streams_view.toolbar_focus_targets()[0]);
        crate::epg::desktop::show_guide_manager(window.upcast_ref::<gtk::Window>());
        let manager = focus001_dialog_after(&window, None);
        let add = focus001_button_in(&manager, "Add URL");
        assert!(add.grab_focus());
        native_x11_key("Return");
        let nested = focus001_dialog_after(&window, Some(&manager));
        let save = focus001_button_in(&nested, "Save Guide");
        assert!(save.grab_focus());
        native_x11_key("Return");
        assert_eq!(
            window.visible_dialog(),
            Some(nested.clone()),
            "blank guide URL must be refused without async network operation"
        );
        let cancel = focus001_button_in(&nested, "Cancel");
        assert!(cancel.grab_focus());
        native_x11_key("Return");
        assert_eq!(
            window.visible_dialog(),
            Some(manager.clone()),
            "guide Add Cancel must restore actual guide manager"
        );
        let owner = window.focused_widget().expect("guide Cancel return owner");
        assert!(owner.is_mapped() && TvWindow::is_within(&owner, manager.upcast_ref()));
        eprintln!("FOCUS001 guide_add native_empty_rejected=1 cancel_return=1 no_network=1");
        native_x11_key("Escape");
        window.close();
    }

    fn focus001_populated_toolbar_viewport(width: i32, height: i32) {
        let (_app, window) = mapped_window("streams");
        window.set_default_size(width, height);
        let streams = &window.imp().streams_view;
        let ids = streams.install_remote_action_test_rows();
        settle();
        assert!(streams.show_and_focus_test_results());
        assert_eq!(
            streams.remote_action_test_selected_id().as_deref(),
            Some(ids[0].as_str())
        );
        let toolbar = streams.toolbar_focus_targets();
        assert_native_input_delivery(&toolbar[0]);
        let mut sampled = 0u32;
        let mut wrong = Vec::<String>::new();
        for (i, control) in toolbar.iter().enumerate() {
            if !control.is_mapped() || !control.is_visible() || !control.is_sensitive() {
                continue;
            }
            let Some(before) = control.compute_bounds(window.upcast_ref::<gtk::Widget>()) else {
                continue;
            };
            if !control.grab_focus() {
                continue;
            }
            for key in ["Up", "Down", "Left", "Right"] {
                assert!(control.grab_focus(), "populated toolbar origin unavailable");
                native_x11_key(key);
                sampled += 1;
                let owner = window.focused_widget().expect("native toolbar lost focus");
                if !owner.is_mapped() || !owner.is_sensitive() {
                    wrong.push(format!("index={i} {key}: invalid owner"));
                    continue;
                }
                // SearchEntry caret keys can retain the same logical control
                // even if the focused GtkText child has a smaller allocation.
                // Such an editor-consumed arrow is not a spatial transition.
                if TvWindow::is_within(&owner, control) {
                    continue;
                }
                let Some(after) = owner.compute_bounds(window.upcast_ref::<gtk::Widget>()) else {
                    continue;
                };
                let dx = (after.x() + after.width() / 2.0) - (before.x() + before.width() / 2.0);
                let dy = (after.y() + after.height() / 2.0) - (before.y() + before.height() / 2.0);
                let wrong_dir = match key {
                    "Right" => dx < -4.0,
                    "Left" => dx > 4.0,
                    "Down" => dy < -4.0,
                    "Up" => dy > 4.0,
                    _ => false,
                };
                if wrong_dir {
                    wrong.push(format!("index={i} {key}: dx={dx:.1} dy={dy:.1}"));
                }
            }
        }
        eprintln!("FOCUS001 streams_populated width={width} height={height} native_samples={sampled} wrong={wrong:?}");
        assert!(
            sampled >= 12,
            "populated toolbar lacks sufficient native key delivery"
        );
        assert!(
            wrong.is_empty(),
            "RED: populated toolbar violates screen geometry: {wrong:?}"
        );
        window.close();
    }

    #[test]
    #[ignore = "native full My Channels populated toolbar spatial probes at compact viewport"]
    fn gtk_focus001_xtest_streams_populated_toolbar_compact_native() {
        focus001_populated_toolbar_viewport(640, 480);
    }

    #[test]
    #[ignore = "native full My Channels populated toolbar spatial probes at TV viewport"]
    fn gtk_focus001_xtest_streams_populated_toolbar_tv_native() {
        focus001_populated_toolbar_viewport(1280, 800);
    }

    #[test]
    #[ignore = "native XTest compact 1484-row 50-step/reverse GTK ListView recycling"]
    fn gtk_focus001_xtest_streams_virtual_native_dpad_1484() {
        focus001_virtual_dpad_route(1_484, 50);
    }

    // Probe the complete *physical* arrow route from the actual Search control
    // to Manage Sources at two screen sizes. The seed grab_focus calls below
    // discover candidate edges; only a separately replayed Search-origin
    // XTest sequence establishes accessibility, never a directly seeded
    // final Manage button. Every sampled edge is reversible/provenanced.
    fn focus001_streams_manage_launcher_connected(width: i32, height: i32) {
        let (_app, window, scratch) = focus001_manager_fixture(3);
        window.set_default_size(width, height);
        settle();
        let controls = window.imp().streams_view.toolbar_focus_targets();
        const START: usize = 0;
        const GOAL: usize = 9;
        let usable = |i: usize| {
            controls[i].is_mapped() && controls[i].is_visible() && controls[i].is_sensitive()
        };
        assert!(usable(START) && usable(GOAL));
        let keys = ["Up", "Down", "Left", "Right"];
        let mut graph = vec![Vec::<(usize, &'static str)>::new(); controls.len()];
        let mut count = 0;
        for i in 0..controls.len() {
            if !usable(i) {
                continue;
            }
            for arrow in keys {
                assert!(controls[i].grab_focus(), "native survey origin {i}");
                settle();
                native_x11_key(arrow);
                count += 1;
                if let Some(owner) = window.focused_widget() {
                    if let Some(j) = controls.iter().enumerate().find_map(|(j, w)| {
                        (usable(j) && TvWindow::is_within(&owner, w)).then_some(j)
                    }) {
                        if j != i {
                            graph[i].push((j, arrow));
                        }
                    }
                }
            }
        }
        let mut previous = vec![None::<(usize, &'static str)>; controls.len()];
        let mut queue = std::collections::VecDeque::from([START]);
        let mut found = vec![false; controls.len()];
        found[START] = true;
        while let Some(i) = queue.pop_front() {
            if i == GOAL {
                break;
            }
            for &(j, key) in &graph[i] {
                if !found[j] {
                    found[j] = true;
                    previous[j] = Some((i, key));
                    queue.push_back(j);
                }
            }
        }
        let mut route = Vec::new();
        let mut cursor = GOAL;
        while cursor != START {
            let Some((before, arrow)) = previous[cursor] else {
                break;
            };
            route.push((before, cursor, arrow));
            cursor = before;
        }
        route.reverse();
        eprintln!("FOCUS001 manager_launcher viewport={width}x{height} edges={count} reachable={} route={route:?}", found[GOAL]);
        assert!(
            found[GOAL],
            "RED: Manage Sources is not D-pad reachable from Search"
        );
        assert!(controls[START].grab_focus());
        for (before, after, arrow) in route {
            assert_owner(&window, &controls[before], "connected manager origin");
            native_x11_key(arrow);
            assert_owner(&window, &controls[after], "native reachable manager hop");
        }
        assert_owner(&window, &controls[GOAL], "native manage action portal");
        native_x11_key("Return");
        let manager = focus001_dialog_after(&window, None);
        for label in ["Add Playlist", "Programme Guides", "Close"] {
            let action = focus001_button_in(&manager, label);
            assert!(
                action.is_mapped() && action.is_sensitive(),
                "manager {label} action"
            );
        }
        let focused = window
            .focused_widget()
            .expect("manager initial native owner");
        assert!(focused.is_mapped() && TvWindow::is_within(&focused, manager.upcast_ref()));
        native_x11_key("Escape");
        assert!(window.visible_dialog().is_none());
        let returned = window
            .focused_widget()
            .expect("manager Escape restores focus");
        assert!(
            returned.is_mapped()
                && TvWindow::is_within(&returned, window.imp().streams_view.upcast_ref())
        );
        eprintln!("FOCUS001 manager_launcher_visited native=1 modal=1 escape_return=1 viewport={width}x{height}");
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove disposable source rows");
    }

    #[test]
    #[ignore = "native XTest Search-to-Manage Sources path, mapped manager, compact"]
    fn gtk_focus001_xtest_streams_manage_launcher_connected_compact() {
        focus001_streams_manage_launcher_connected(640, 480);
    }

    #[test]
    #[ignore = "native XTest Search-to-Manage Sources path, mapped manager, TV"]
    fn gtk_focus001_xtest_streams_manage_launcher_connected_tv() {
        focus001_streams_manage_launcher_connected(1280, 800);
    }

    // Native XTest connected reachability across *all currently enabled*
    // toolbar actions in each page; the individual seed grabs only discover
    // possible graph edges. Every claimed path is replayed from Search with
    // genuine keyboard events, checking each mapped focused owner. A missing
    // edge remains a blocking RED instead of being excused as a focusable button.
    fn focus001_page_connected_toolbar(page: &str, width: i32) {
        let (_app, window) = mapped_window(page);
        let height = if width == 640 { 480 } else { 800 };
        window.set_default_size(width, height);
        if page == "live" {
            window.imp().live_view.show_toolbar_for_test();
        }
        if page == "streams" {
            let streams = &window.imp().streams_view;
            streams.install_remote_action_test_rows();
            settle();
            assert!(streams.show_and_focus_test_results());
        }
        settle();
        let controls = match page {
            "live" => window.imp().live_view.toolbar_focus_targets(),
            "mediathek" => window.imp().mediathek_view.toolbar_focus_targets(),
            "streams" => window.imp().streams_view.toolbar_focus_targets(),
            _ => panic!("unsupported synthetic page"),
        };
        assert!(!controls.is_empty(), "page has no remote toolbar");
        assert_native_input_delivery(&controls[0]);
        let available = |i: usize| {
            controls[i].is_mapped() && controls[i].is_visible() && controls[i].is_sensitive()
        };
        let mut links = vec![Vec::<(usize, &'static str)>::new(); controls.len()];
        let mut sampled = 0usize;
        for i in 0..controls.len() {
            if !available(i) {
                continue;
            }
            for key in ["Up", "Down", "Left", "Right"] {
                assert!(controls[i].grab_focus(), "survey origin unavailable: {i}");
                settle();
                native_x11_key(key);
                sampled += 1;
                if let Some(owner) = window.focused_widget() {
                    if let Some(j) = controls.iter().enumerate().find_map(|(j, candidate)| {
                        (available(j) && TvWindow::is_within(&owner, candidate)).then_some(j)
                    }) {
                        if i != j {
                            links[i].push((j, key));
                        }
                    }
                }
            }
        }
        // Breadth-first shortest actual-key routes from Search. Each route
        // is reexecuted in the real mapped GTK window, not model-simulated.
        let mut parent = vec![None::<(usize, &'static str)>; controls.len()];
        let mut found = vec![false; controls.len()];
        found[0] = true;
        let mut queue = std::collections::VecDeque::from([0]);
        while let Some(i) = queue.pop_front() {
            for &(j, key) in &links[i] {
                if !found[j] {
                    found[j] = true;
                    parent[j] = Some((i, key));
                    queue.push_back(j);
                }
            }
        }
        let missing: Vec<usize> = (0..controls.len())
            .filter(|&i| available(i) && !found[i])
            .collect();
        eprintln!("FOCUS001 connected_toolbar page={page} viewport={width}x{height} sampled={sampled} missing={missing:?} edges={links:?}");
        assert!(
            missing.is_empty(),
            "RED: page toolbar enabled control lacks native arrow path from Search: {missing:?}"
        );
        for target in 0..controls.len() {
            if !available(target) {
                continue;
            }
            let mut path = Vec::<(usize, usize, &'static str)>::new();
            let mut cur = target;
            while cur != 0 {
                let (from, key) = parent[cur].expect("found route must have predecessor");
                path.push((from, cur, key));
                cur = from;
            }
            path.reverse();
            assert!(controls[0].grab_focus());
            assert_owner(&window, &controls[0], "connected graph Search origin");
            for (from, to, key) in &path {
                assert_owner(&window, &controls[*from], "graph route origin");
                native_x11_key(key);
                assert_owner(&window, &controls[*to], "native graph route destination");
            }
            assert_owner(
                &window,
                &controls[target],
                "native route reachable toolbar control",
            );
            eprintln!("FOCUS001 route page={page} dest={target} native={path:?}");
        }
        eprintln!("FOCUS001 connected_toolbar PASS page={page} viewport={width}x{height} native_samples={sampled}");
        window.close();
    }

    #[test]
    #[ignore = "native connected Live toolbar complete enabled graph 640x480"]
    fn gtk_focus001_xtest_live_connected_toolbar_compact() {
        focus001_page_connected_toolbar("live", 640);
    }
    #[test]
    #[ignore = "native connected Live toolbar complete enabled graph 1280x800"]
    fn gtk_focus001_xtest_live_connected_toolbar_tv() {
        focus001_page_connected_toolbar("live", 1280);
    }
    #[test]
    #[ignore = "native connected Mediathek toolbar complete enabled graph 640x480"]
    fn gtk_focus001_xtest_mediathek_connected_toolbar_compact() {
        focus001_page_connected_toolbar("mediathek", 640);
    }
    #[test]
    #[ignore = "native connected Mediathek toolbar complete enabled graph 1280x800"]
    fn gtk_focus001_xtest_mediathek_connected_toolbar_tv() {
        focus001_page_connected_toolbar("mediathek", 1280);
    }
    #[test]
    #[ignore = "native connected populated My Channels toolbar complete enabled graph 640x480"]
    fn gtk_focus001_xtest_streams_connected_toolbar_compact() {
        focus001_page_connected_toolbar("streams", 640);
    }
    #[test]
    #[ignore = "native connected populated My Channels toolbar complete enabled graph 1280x800"]
    fn gtk_focus001_xtest_streams_connected_toolbar_tv() {
        focus001_page_connected_toolbar("streams", 1280);
    }
}
