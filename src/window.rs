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

    // Real Search -> Manage Sources -> Switch journey. The route is an
    // observed native path, never a direct GtkSwitch grab_focus shortcut.
    fn focus001_connected_source_switch(count: u32) {
        let (_app, window, scratch) = focus001_manager_fixture(count);
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert!(search.grab_focus());
        for key in ["Down", "Down", "Down", "Right"] {
            native_x11_key(key);
        }
        let launcher = window.focused_widget().expect("native manager launcher");
        assert!(launcher.is_mapped() && launcher.is_sensitive());
        native_x11_key("Return");
        let manager = focus001_dialog_after(&window, None);
        let mut switches = Vec::<gtk::Switch>::new();
        let mut queue = vec![manager.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = queue.pop() {
            if let Some(button) = widget.downcast_ref::<gtk::Switch>() {
                if button.is_mapped() && button.is_sensitive() {
                    switches.push(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(w) = child {
                child = w.next_sibling();
                queue.push(w);
            }
        }
        assert_eq!(switches.len(), count as usize);
        let entry = window.focused_widget().expect("source dialog native focus");
        assert!(entry.is_mapped() && TvWindow::is_within(&entry, manager.upcast_ref()));
        let route = ["Down", "Right"];
        for key in route {
            native_x11_key(key);
        }
        let owner = window.focused_widget().expect("remote switch focus owner");
        let switch = switches
            .iter()
            .find(|s| TvWindow::is_within(&owner, s.upcast_ref()))
            .expect("RED: mapped source Switch inaccessible by native arrow route");
        assert!(owner.is_mapped() && owner.is_sensitive());
        let prior = switch.is_active();
        assert!(prior, "all disposable sources start enabled");
        let store = crate::streams::storage::Storage::at(scratch.join("manager.sqlite"));
        let before = store.list_sources().expect("prior DB rows");
        assert_eq!(before.len(), count as usize);
        native_x11_key("space");
        for _ in 0..70 {
            settle();
            if !switch.is_active() {
                break;
            }
        }
        assert!(
            !switch.is_active(),
            "RED: native Space did not toggle source switch"
        );
        for _ in 0..70 {
            settle();
            if store
                .list_sources()
                .expect("after toggle")
                .iter()
                .filter(|x| x.enabled)
                .count()
                == count as usize - 1
            {
                break;
            }
        }
        let after = store
            .list_sources()
            .expect("persisted source switch toggle");
        assert_eq!(after.len(), before.len(), "toggle must not delete a source");
        let mutated: Vec<_> = before
            .iter()
            .filter(|original| {
                after
                    .iter()
                    .find(|current| current.id == original.id)
                    .is_some_and(|current| current.enabled != original.enabled)
            })
            .collect();
        assert_eq!(
            mutated.len(),
            1,
            "only the chosen source may change enabled state"
        );
        assert_eq!(
            after.iter().filter(|x| x.enabled).count(),
            count as usize - 1,
            "RED: toggled source state not persisted to disposable SQLite"
        );
        let focus_after = window
            .focused_widget()
            .expect("RED: switch rebind lost focus owner");
        assert!(
            focus_after.is_mapped()
                && focus_after.is_sensitive()
                && TvWindow::is_within(&focus_after, manager.upcast_ref()),
            "RED: source switch async reload lost manager focus"
        );
        if count == 1 {
            assert!(
                TvWindow::find_descendant(manager.upcast_ref(), &|w| {
                    w.downcast_ref::<gtk::Switch>().is_some_and(|s| {
                        s.is_mapped() && TvWindow::is_within(&focus_after, s.upcast_ref())
                    })
                })
                .is_some(),
                "RED: one-source toggle must restore focus to the same Switch"
            );
            native_x11_key("space");
            for _ in 0..70 {
                settle();
                if store.list_sources().expect("reverse source toggle")[0].enabled {
                    break;
                }
            }
            assert!(
                store.list_sources().expect("persisted reverse toggle")[0].enabled,
                "RED: remote source Switch reverse toggle not persisted"
            );
        }
        eprintln!("FOCUS001 source_switch_native count={count} keys=Down,Down,Down,Right,Return,Down,Right,space sqlite=ok focus=ok reverse={}",count==1);
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove synthetic DB fixture");
    }

    #[test]
    #[ignore = "real XTest manager portal, source switch navigation and SQLite persistence"]
    fn gtk_focus001_xtest_source_switch_one_native_persist() {
        focus001_connected_source_switch(1);
    }

    #[test]
    #[ignore = "real XTest three-source manager switch navigation and SQLite persistence"]
    fn gtk_focus001_xtest_source_switch_three_native_persist() {
        focus001_connected_source_switch(3);
    }

    // A separate real-key upper-row graph check, not a programmatic lower
    // button focus seed. A failure is a directional navigation RED; unchanged
    // runtime handlers are deliberately not corrected by this test worker.
    fn focus001_source_manager_upper_graph(width: i32) {
        let (_app, window, scratch) = focus001_manager_fixture(3);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert!(search.grab_focus());
        for key in ["Down", "Down", "Down", "Right", "Return"] {
            native_x11_key(key);
        }
        let manager = focus001_dialog_after(&window, None);
        for key in ["Down", "Right"] {
            native_x11_key(key);
        }
        let owner = window.focused_widget().expect("native upper row start");
        assert!(
            TvWindow::find_descendant(manager.upcast_ref(), &|widget| {
                widget
                    .downcast_ref::<gtk::Switch>()
                    .is_some_and(|s| s.is_mapped() && TvWindow::is_within(&owner, s.upcast_ref()))
            })
            .is_some(),
            "RED: remote could not reach source upper-row first switch"
        );
        for label in ["Refresh", "Defaults", "Rename"] {
            native_x11_key("Right");
            let focus = window
                .focused_widget()
                .expect("native upper action destination");
            let button = TvWindow::find_descendant(manager.upcast_ref(), &|widget| {
                widget.downcast_ref::<gtk::Button>().is_some_and(|button| {
                    button.is_mapped()
                        && button.label().as_deref() == Some(label)
                        && TvWindow::is_within(&focus, button.upcast_ref())
                })
            });
            assert!(
                button.is_some(),
                "RED: Right did not reach expected source upper action {label}"
            );
            assert!(TvWindow::is_within(&focus, manager.upcast_ref()));
        }
        for label in ["Defaults", "Refresh"] {
            native_x11_key("Left");
            let focus = window.focused_widget().expect("native upper reverse owner");
            assert!(
                TvWindow::find_descendant(manager.upcast_ref(), &|widget| {
                    widget.downcast_ref::<gtk::Button>().is_some_and(|button| {
                        button.is_mapped()
                            && button.label().as_deref() == Some(label)
                            && TvWindow::is_within(&focus, button.upcast_ref())
                    })
                })
                .is_some(),
                "RED: Left does not reverse to {label}"
            );
        }
        eprintln!(
            "FOCUS001 source_manager_upper_graph viewport={width} forward=3 reverse=2 native=1"
        );
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove disposable manager graph DB");
    }

    #[test]
    #[ignore = "real XTest source manager upper action row forward/reverse at 640x480"]
    fn gtk_focus001_xtest_source_manager_upper_graph_compact() {
        focus001_source_manager_upper_graph(640);
    }

    #[test]
    #[ignore = "real XTest source manager upper action row forward/reverse at 1280x800"]
    fn gtk_focus001_xtest_source_manager_upper_graph_tv() {
        focus001_source_manager_upper_graph(1280);
    }

    // Source-switch restoration is an independent scenario from the RED
    // immediate-focus-reversal check: explicitly leave and reenter the manager
    // using native keys; assert the disabled DB state is reflected in the
    // newly mapped control, then reenable it via Space.
    #[test]
    #[ignore = "native one-source persisted reversal after Escape and manager reentry"]
    fn gtk_focus001_xtest_source_switch_one_reopen_reversal() {
        let (_app, window, scratch) = focus001_manager_fixture(1);
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        let store = crate::streams::storage::Storage::at(scratch.join("manager.sqlite"));
        for expected_before in [true, false] {
            assert!(search.grab_focus());
            for key in ["Down", "Down", "Down", "Right", "Return"] {
                native_x11_key(key);
            }
            let manager = focus001_dialog_after(&window, None);
            for key in ["Down", "Right"] {
                native_x11_key(key);
            }
            let owner = window
                .focused_widget()
                .expect("native reopened source switch owner");
            let switch = TvWindow::find_descendant(manager.upcast_ref(), &|widget| {
                widget
                    .downcast_ref::<gtk::Switch>()
                    .is_some_and(|candidate| {
                        candidate.is_mapped()
                            && candidate.is_sensitive()
                            && TvWindow::is_within(&owner, candidate.upcast_ref())
                    })
            })
            .expect("RED: manager-reopened switch not reachable using native arrows")
            .downcast::<gtk::Switch>()
            .expect("GtkSwitch");
            assert_eq!(
                switch.is_active(),
                expected_before,
                "RED: reopened native source switch must reflect persisted SQLite state"
            );
            native_x11_key("space");
            for _ in 0..70 {
                settle();
                if store.list_sources().expect("toggle state query")[0].enabled != expected_before {
                    break;
                }
            }
            assert_ne!(
                store.list_sources().expect("persisted native toggle")[0].enabled,
                expected_before,
                "RED: native source switch toggle not persisted"
            );
            native_x11_key("Escape");
            assert!(
                window.visible_dialog().is_none(),
                "Escape did not close manager"
            );
        }
        assert!(
            store.list_sources().expect("reversed source state")[0].enabled,
            "source must be reenabled after a second native Space"
        );
        eprintln!(
            "FOCUS001 source_switch_reopen native_escapes=2 native_toggles=2 SQLite_restored=1"
        );
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove disposable source fixture");
    }

    fn focus001_source_manager_lower_graph(width: i32) {
        let (_app, window, scratch) = focus001_manager_fixture(3);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert!(search.grab_focus());
        for key in [
            "Down", "Down", "Down", "Right", "Return", "Down", "Right", "Right",
        ] {
            native_x11_key(key);
        }
        let manager = focus001_dialog_after(&window, None);
        let start = window
            .focused_widget()
            .expect("native Refresh upper-row owner");
        assert!(
            TvWindow::find_descendant(manager.upcast_ref(), &|w| {
                w.downcast_ref::<gtk::Button>().is_some_and(|b| {
                    b.is_mapped()
                        && b.label().as_deref() == Some("Refresh")
                        && TvWindow::is_within(&start, b.upcast_ref())
                })
            })
            .is_some(),
            "RED: first upper action Refresh not D-pad reachable"
        );
        native_x11_key("Down");
        let lower = window.focused_widget().expect("native lower action owner");
        let lower_button = TvWindow::find_descendant(manager.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>().is_some_and(|b| {
                b.is_mapped()
                    && b.is_sensitive()
                    && b.label()
                        .as_deref()
                        .is_some_and(|s| matches!(s, "Replace" | "Delete" | "↑" | "↓"))
                    && TvWindow::is_within(&lower, b.upcast_ref())
            })
        });
        eprintln!("FOCUS001 lower_row_probe viewport={width} native_down_owner={} mapped={} in_manager={} label={:?}",
            lower.type_().name(), lower.is_mapped(), TvWindow::is_within(&lower, manager.upcast_ref()),
            lower.downcast_ref::<gtk::Button>().and_then(|button| button.label()));
        assert!(
            lower_button.is_some(),
            "RED: Down from upper source action cannot reach lower action row"
        );
        native_x11_key("Right");
        let right = window
            .focused_widget()
            .expect("lower-row Right destination");
        assert!(right.is_mapped() && TvWindow::is_within(&right, manager.upcast_ref()));
        assert!(
            !TvWindow::is_within(&right, &lower),
            "RED: lower-row Right failed to advance"
        );
        native_x11_key("Left");
        let returned = window.focused_widget().expect("lower-row reverse owner");
        assert!(
            TvWindow::is_within(&returned, &lower),
            "RED: lower-row Right/Left does not reverse"
        );
        eprintln!("FOCUS001 source_manager_lower_graph viewport={width} down_right_left_reverse=1");
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove disposable manager source fixture");
    }

    #[test]
    #[ignore = "native three-source manager lower actions and Right/Left reversal 640x480"]
    fn gtk_focus001_xtest_source_manager_lower_graph_compact() {
        focus001_source_manager_lower_graph(640);
    }

    #[test]
    #[ignore = "native three-source manager lower actions and Right/Left reversal 1280x800"]
    fn gtk_focus001_xtest_source_manager_lower_graph_tv() {
        focus001_source_manager_lower_graph(1280);
    }
    /// Real native Search-to-Manage Sources route. The actual toolbar index is
    /// only an assertion target, not a focus seed. Down is delivered via XTest.
    fn focus001_recovery_open_manager_native(window: &TvWindow) -> adw::Dialog {
        let targets = window.imp().streams_view.toolbar_focus_targets();
        let search = targets[0].clone();
        let manage = targets[9].clone();
        assert_native_input_delivery(&search);
        // Repeated Down alone is NOT the route to Manage Sources: the
        // geometric toolbar has horizontal transitions. Survey each mapped
        // origin with real XTest, then replay a connected Search-origin
        // native path (never seed the final Manage action).
        let usable = |i: usize| {
            targets[i].is_mapped() && targets[i].is_visible() && targets[i].is_sensitive()
        };
        assert!(usable(0) && usable(9));
        let mut edges = vec![Vec::<(usize, &'static str)>::new(); targets.len()];
        for i in 0..targets.len() {
            if !usable(i) {
                continue;
            }
            for key in ["Up", "Down", "Left", "Right"] {
                assert!(targets[i].grab_focus(), "survey initial toolbar origin {i}");
                native_x11_key(key);
                if let Some(j) = window.focused_widget().and_then(|owner| {
                    targets.iter().enumerate().find_map(|(j, item)| {
                        (usable(j) && TvWindow::is_within(&owner, item)).then_some(j)
                    })
                }) {
                    if j != i {
                        edges[i].push((j, key));
                    }
                }
            }
        }
        let mut previous = vec![None::<(usize, &'static str)>; targets.len()];
        let mut queue = std::collections::VecDeque::from([0usize]);
        let mut discovered = vec![false; targets.len()];
        discovered[0] = true;
        while let Some(i) = queue.pop_front() {
            if i == 9 {
                break;
            }
            for &(j, key) in &edges[i] {
                if !discovered[j] {
                    discovered[j] = true;
                    previous[j] = Some((i, key));
                    queue.push_back(j);
                }
            }
        }
        let mut reverse_route = Vec::<(usize, usize, &'static str)>::new();
        let mut pos = 9;
        while pos != 0 {
            let Some((origin, key)) = previous[pos] else {
                break;
            };
            reverse_route.push((origin, pos, key));
            pos = origin;
        }
        reverse_route.reverse();
        eprintln!(
            "FOCUS001 recovered native manager entrance connected={} route={reverse_route:?}",
            discovered[9]
        );
        assert!(
            discovered[9],
            "RED: Manage Sources is not natively reachable from Search"
        );
        assert!(search.grab_focus());
        for (origin, target, key) in reverse_route {
            assert_owner(window, &targets[origin], "native manager route origin");
            native_x11_key(key);
            assert_owner(window, &targets[target], "native manager route destination");
        }
        assert_owner(window, &manage, "native Manage Sources entry");
        native_x11_key("Return");
        let manager = focus001_dialog_after(window, None);
        for label in ["Add Playlist", "Programme Guides", "Close"] {
            let action = focus001_button_in(&manager, label);
            assert!(
                action.is_mapped() && action.is_sensitive(),
                "RED: native Manage Sources dialog lost {label}"
            );
        }
        manager
    }

    #[test]
    #[ignore = "native real connected source manager grid two rows compact"]
    fn gtk_focus001_recovery_source_graph_2_compact() {
        focus001_recovery_manager_graph(2, 640);
    }
    #[test]
    #[ignore = "native real connected source manager grid two rows TV"]
    fn gtk_focus001_recovery_source_graph_2_tv() {
        focus001_recovery_manager_graph(2, 1280);
    }

    fn focus001_recovery_guide_nested(count: usize, width: i32) {
        crate::epg::desktop::focus001_seed_disposable_guides(count);
        let (_app, window, scratch) = focus001_manager_fixture(0);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let source_manager = focus001_recovery_open_manager_native(&window);
        let button = focus001_button_in(&source_manager, "Programme Guides");
        for _ in 0..5 {
            if window
                .focused_widget()
                .is_some_and(|owner| TvWindow::is_within(&owner, button.upcast_ref()))
            {
                break;
            }
            native_x11_key("Right");
        }
        let focused = window
            .focused_widget()
            .expect("native Programme Guides launcher owner");
        assert!(
            TvWindow::is_within(&focused, button.upcast_ref()),
            "RED: source manager cannot reach Programme Guides through real Right key"
        );
        native_x11_key("Return");
        let guide_manager = focus001_dialog_after(&window, Some(&source_manager));
        focus001_graph_root_controls(&window, &guide_manager, false, count, false);
        native_x11_key("Escape");
        assert!(
            window.visible_dialog() != Some(guide_manager.clone()),
            "RED: native Escape cannot dismiss the nested guide manager"
        );
        eprintln!("FOCUS001 NESTED_GUIDE count={count} viewport={width} true_source_portal=1 internal_graph=1");
        window.close();
        std::fs::remove_dir_all(scratch).expect("disposable source DB cleanup");
    }
    #[test]
    #[ignore = "native nested Programme Guides action from source manager"]
    fn gtk_focus001_recovery_nested_guide_0_compact() {
        focus001_recovery_guide_nested(0, 640);
    }
    #[test]
    #[ignore = "native nested Programme Guides action from source manager"]
    fn gtk_focus001_recovery_nested_guide_0_tv() {
        focus001_recovery_guide_nested(0, 1280);
    }
    #[test]
    #[ignore = "native nested Programme Guides action from source manager"]
    fn gtk_focus001_recovery_nested_guide_3_compact() {
        focus001_recovery_guide_nested(3, 640);
    }
    #[test]
    #[ignore = "native nested Programme Guides action from source manager"]
    fn gtk_focus001_recovery_nested_guide_3_tv() {
        focus001_recovery_guide_nested(3, 1280);
    }
    // Native-rendered pixels, with an explicit synthetic positive ring and
    // intentionally suppressed negative control.  No screenshots are stored.
    fn focus001_recovery_painted_toolbar(page: &str, width: i32) {
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
        let search = &controls[0];
        let target = &controls[1];
        assert_native_input_delivery(search);
        assert!(search.is_mapped() && target.is_mapped());
        let bounds = target
            .compute_bounds(window.upcast_ref::<gtk::Widget>())
            .expect("mapped toolbar target must have screen geometry");
        let capture = || {
            let exe = std::env::var_os("FOCUS001_PIXEL_BIN").expect("native pixel oracle required");
            let params = [
                "--pixels".to_string(),
                (bounds.x() - 3.0).max(0.0).to_string(),
                (bounds.y() - 3.0).max(0.0).to_string(),
                (bounds.width() + 6.0).to_string(),
                (bounds.height() + 6.0).to_string(),
            ];
            let result = std::process::Command::new(exe)
                .args(&params)
                .output()
                .expect("capture in-memory X11 pixels");
            assert!(
                result.status.success(),
                "PIXEL_ENVIRONMENT capture exit {:?}",
                result.status.code()
            );
            String::from_utf8(result.stdout)
                .expect("UTF8 pixel digest")
                .trim()
                .to_owned()
        };
        assert!(search.grab_focus());
        settle();
        let unfocused = capture();
        native_x11_key("Right");
        assert_owner(&window, target, "native Search Right toolbar focus");
        let focused = capture();
        let provider = gtk::CssProvider::new();
        provider.load_from_string(".focus001-positive:focus {outline: 4px solid rgb(255,0,255);outline-offset: -4px;box-shadow:none;} .focus001-negative,.focus001-negative:focus {outline:none;outline-offset:0;box-shadow:none;}");
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("X11 display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        target.add_css_class("focus001-positive");
        assert!(search.grab_focus());
        settle();
        let synthetic_before = capture();
        native_x11_key("Right");
        assert_owner(&window, target, "native positive pixel focus");
        let synthetic_after = capture();
        assert_ne!(
            synthetic_before, synthetic_after,
            "RED: even a deliberately visible focus outline produces no pixel change"
        );
        target.remove_css_class("focus001-positive");
        target.add_css_class("focus001-negative");
        assert!(search.grab_focus());
        settle();
        let suppressed_before = capture();
        native_x11_key("Right");
        assert_owner(&window, target, "native deliberately suppressed focus");
        let suppressed_after = capture();
        assert_eq!(
            suppressed_before, suppressed_after,
            "RED: suppressed focus ring negative control still changes pixels"
        );
        eprintln!("FOCUS001 PAINT page={page} width={width} before={unfocused} after={focused} positive_before={synthetic_before} positive_after={synthetic_after} negative_identical={} ", suppressed_before == suppressed_after);
        assert_ne!(
            unfocused, focused,
            "RED: actual app native focus does not visibly change toolbar button pixels"
        );
        window.close();
    }
    #[test]
    #[ignore = "native XTest painted focus positive and suppressed negative control"]
    fn gtk_focus001_recovery_painted_live_compact() {
        focus001_recovery_painted_toolbar("live", 640);
    }
    #[test]
    #[ignore = "native XTest painted focus positive and suppressed negative control"]
    fn gtk_focus001_recovery_painted_live_tv() {
        focus001_recovery_painted_toolbar("live", 1280);
    }
    #[test]
    #[ignore = "native XTest painted focus positive and suppressed negative control"]
    fn gtk_focus001_recovery_painted_mediathek_compact() {
        focus001_recovery_painted_toolbar("mediathek", 640);
    }
    #[test]
    #[ignore = "native XTest painted focus positive and suppressed negative control"]
    fn gtk_focus001_recovery_painted_mediathek_tv() {
        focus001_recovery_painted_toolbar("mediathek", 1280);
    }
    #[test]
    #[ignore = "native XTest painted focus positive and suppressed negative control"]
    fn gtk_focus001_recovery_painted_streams_compact() {
        focus001_recovery_painted_toolbar("streams", 640);
    }
    #[test]
    #[ignore = "native XTest painted focus positive and suppressed negative control"]
    fn gtk_focus001_recovery_painted_streams_tv() {
        focus001_recovery_painted_toolbar("streams", 1280);
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

    // Connected remote graph survey: the modal entry is reached with XTest
    // from the real My Channels search field. Only local edges may be sampled
    // from a seeded control; global reachability is proven from the genuine
    // focused entry vertex, not from grab_focus on the destination.
    // One real XTest process can enqueue up to 8 physical key pairs. GTK
    // handles them in order; call settle() before checking every chunk.
    fn focus001_native_repeat(key: &str, count: u32) {
        assert!((1..=8).contains(&count));
        let emitter = std::env::var_os("FOCUS001_XTEST_BIN").expect("real XTest emitter");
        let output = std::process::Command::new(emitter)
            .arg(key)
            .arg(count.to_string())
            .output()
            .expect("emit bounded native XTest key burst");
        assert!(
            output.status.success(),
            "native {key} repeat={count} failed: {:?}",
            output.status.code()
        );
        settle();
    }

    fn focus001_graph_root_controls(
        window: &TvWindow,
        dialog: &adw::Dialog,
        source_mode: bool,
        expected_count: usize,
        replay_from_entry: bool,
    ) {
        let allowed: &[&str] = if source_mode {
            &[
                "Add Playlist",
                "Programme Guides",
                "Close",
                "Refresh",
                "Defaults",
                "Rename",
                "Replace",
                "Delete",
                "↑",
                "↓",
            ]
        } else {
            &[
                "Add URL",
                "Choose Local XMLTV",
                "Close",
                "Use as Shared Default",
                "Refresh",
                "Edit",
                "Remove",
            ]
        };
        let mut controls: Vec<(String, gtk::Widget)> = Vec::new();
        let mut stack = vec![dialog.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = stack.pop() {
            if widget.is_mapped() && widget.is_sensitive() {
                if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                    if let Some(label) = button.label() {
                        if allowed.contains(&label.as_str()) {
                            controls.push((label.to_string(), widget.clone()));
                        }
                    }
                } else if source_mode && widget.is::<gtk::Switch>() {
                    controls.push(("Switch".to_string(), widget.clone()));
                }
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                child = next.next_sibling();
                stack.push(next);
            }
        }
        if controls.len() < 3 {
            let mut labels = Vec::new();
            let mut nodes = vec![dialog.clone().upcast::<gtk::Widget>()];
            while let Some(node) = nodes.pop() {
                if let Some(button) = node.downcast_ref::<gtk::Button>() {
                    labels.push(format!(
                        "{} mapped={} sensitive={}",
                        button.label().unwrap_or_default(),
                        node.is_mapped(),
                        node.is_sensitive()
                    ));
                }
                let mut child = node.first_child();
                while let Some(item) = child {
                    child = item.next_sibling();
                    nodes.push(item);
                }
            }
            eprintln!(
                "FOCUS001 MANAGER DEBUG source_mode={source_mode} controls={} labels={labels:?}",
                controls.len()
            );
        }
        assert!(
            controls.len() >= 3,
            "RED: mapped manager lacks core controls"
        );
        let owners = |owner: &gtk::Widget| {
            controls
                .iter()
                .enumerate()
                .find_map(|(i, (_, target))| TvWindow::is_within(owner, target).then_some(i))
        };
        let focused = window.focused_widget().expect("native dialog entry owner");
        let start =
            owners(&focused).expect("RED: native manager entry cannot reach an enabled control");
        let mut edges: Vec<Vec<(usize, &'static str)>> = vec![Vec::new(); controls.len()];
        let mut failures = Vec::<String>::new();
        for i in 0..controls.len() {
            let (label, widget) = &controls[i];
            assert!(
                widget.grab_focus(),
                "mapped member must be focusable: {label}"
            );
            for key in ["Up", "Down", "Left", "Right"] {
                assert!(
                    widget.grab_focus(),
                    "cannot seed local graph edge for {label}"
                );
                native_x11_key(key);
                if let Some(to) = window.focused_widget().and_then(|w| owners(&w)) {
                    if to != i && !edges[i].iter().any(|(index, _)| *index == to) {
                        edges[i].push((to, key));
                    }
                } else if window
                    .focused_widget()
                    .is_none_or(|w| !TvWindow::is_within(&w, dialog.upcast_ref()))
                {
                    failures.push(format!(
                        "{label} {key} left the actual mapped manager dialog"
                    ));
                }
            }
        }
        let mut seen = vec![false; controls.len()];
        let mut parent = vec![None::<(usize, &'static str)>; controls.len()];
        seen[start] = true;
        let mut queue = std::collections::VecDeque::from([start]);
        while let Some(index) = queue.pop_front() {
            for &(to, key) in &edges[index] {
                if !seen[to] {
                    seen[to] = true;
                    parent[to] = Some((index, key));
                    queue.push_back(to);
                }
            }
        }
        let unreachable: Vec<_> = controls
            .iter()
            .enumerate()
            .filter_map(|(i, (name, _))| (!seen[i]).then_some(format!("{i}:{name}")))
            .collect();
        eprintln!("FOCUS001 recovered graph source={source_mode} fixture={expected_count} entry={} vertices={} seen={} unreachable={unreachable:?} exits={failures:?}", controls[start].0, controls.len(), seen.iter().filter(|x| **x).count());
        assert!(
            failures.is_empty(),
            "RED: arrow escaped the modal's mapped controls"
        );
        assert!(
            unreachable.is_empty(),
            "RED: enabled mapped controls disconnected from native entry"
        );
        if replay_from_entry {
            // One-hop seeded survey does not establish a connected D-pad
            // route. Replay every route, including its physical reverse,
            // from the authentic entry focus before claiming closure.
            let opposite = |key: &str| match key {
                "Up" => "Down",
                "Down" => "Up",
                "Left" => "Right",
                "Right" => "Left",
                _ => unreachable!(),
            };
            let mut executed_routes = 0usize;
            for destination in 0..controls.len() {
                let mut route = Vec::<(usize, usize, &'static str)>::new();
                let mut pos = destination;
                while pos != start {
                    let (from, key) = parent[pos].expect("BFS parent");
                    route.push((from, pos, key));
                    pos = from;
                }
                route.reverse();
                assert!(
                    controls[start].1.grab_focus(),
                    "authentic entry remains mapped"
                );
                assert_owner(window, &controls[start].1, "guide route entry");
                for &(from, to, key) in &route {
                    assert_owner(window, &controls[from].1, "guide route origin");
                    native_x11_key(key);
                    assert_owner(window, &controls[to].1, "guide route destination");
                }
                for &(from, to, key) in route.iter().rev() {
                    assert_owner(window, &controls[to].1, "guide reverse origin");
                    native_x11_key(opposite(key));
                    assert_owner(window, &controls[from].1, "guide reverse destination");
                }
                executed_routes += 1;
            }
            eprintln!("FOCUS001 connected_guide_replay controls={} full_native_routes={} with_reverse=true", controls.len(), executed_routes);
        }
    }

    fn focus001_recovery_manager_graph(source_count: u32, width: i32) {
        let (_app, window, scratch) = focus001_manager_fixture(source_count);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let dialog = focus001_recovery_open_manager_native(&window);
        focus001_graph_root_controls(&window, &dialog, true, source_count as usize, false);
        window.close();
        std::fs::remove_dir_all(scratch).expect("cleanup disposable source data");
    }

    fn focus001_recovery_guide_graph(guide_count: usize, width: i32) {
        crate::epg::desktop::focus001_seed_disposable_guides(guide_count);
        let (_app, window) = mapped_window("streams");
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        // Opening the guide manager uses a real native entry sequence.
        crate::epg::desktop::show_guide_manager(window.upcast_ref::<gtk::Window>());
        let dialog = focus001_dialog_after(&window, None);
        focus001_graph_root_controls(&window, &dialog, false, guide_count, false);
        window.close();
    }

    #[test]
    #[ignore = "native XTest source-manager connected graph 0 sources compact"]
    fn gtk_focus001_recovery_source_graph_0_compact() {
        focus001_recovery_manager_graph(0, 640);
    }
    #[test]
    #[ignore = "native XTest source-manager connected graph 1 source compact"]
    fn gtk_focus001_recovery_source_graph_1_compact() {
        focus001_recovery_manager_graph(1, 640);
    }
    #[test]
    #[ignore = "native XTest source-manager connected graph 3 sources compact"]
    fn gtk_focus001_recovery_source_graph_3_compact() {
        focus001_recovery_manager_graph(3, 640);
    }
    #[test]
    #[ignore = "native XTest source-manager connected graph 0 sources TV"]
    fn gtk_focus001_recovery_source_graph_0_tv() {
        focus001_recovery_manager_graph(0, 1280);
    }
    #[test]
    #[ignore = "native XTest source-manager connected graph 1 source TV"]
    fn gtk_focus001_recovery_source_graph_1_tv() {
        focus001_recovery_manager_graph(1, 1280);
    }
    #[test]
    #[ignore = "native XTest source-manager connected graph 3 sources TV"]
    fn gtk_focus001_recovery_source_graph_3_tv() {
        focus001_recovery_manager_graph(3, 1280);
    }
    #[test]
    #[ignore = "native XTest XMLTV-guide manager connected graph 0 guides compact"]
    fn gtk_focus001_recovery_guide_graph_0_compact() {
        focus001_recovery_guide_graph(0, 640);
    }
    #[test]
    #[ignore = "native XTest XMLTV-guide manager connected graph 3 guides compact"]
    fn gtk_focus001_recovery_guide_graph_3_compact() {
        focus001_recovery_guide_graph(3, 640);
    }
    #[test]
    #[ignore = "native XTest XMLTV-guide manager connected graph 0 guides TV"]
    fn gtk_focus001_recovery_guide_graph_0_tv() {
        focus001_recovery_guide_graph(0, 1280);
    }
    #[test]
    #[ignore = "native XTest XMLTV-guide manager connected graph 3 guides TV"]
    fn gtk_focus001_recovery_guide_graph_3_tv() {
        focus001_recovery_guide_graph(3, 1280);
    }
    // Recovery cohort: actual remote OK opens each page's real keyboard.
    // Only the independently seeded starting key is a test hook; each vertical
    // edge, reverse and modal-containment observation uses real XTest events.
    fn focus001_recovery_osk_vertical(page: &str, width: i32) {
        let (_app, window) = mapped_window(page);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        if page == "live" {
            window.imp().live_view.show_toolbar_for_test();
        }
        settle();
        let toolbar = match page {
            "live" => window.imp().live_view.toolbar_focus_targets(),
            "mediathek" => window.imp().mediathek_view.toolbar_focus_targets(),
            "streams" => window.imp().streams_view.toolbar_focus_targets(),
            _ => unreachable!(),
        };
        let source = toolbar[0].clone();
        assert_native_input_delivery(&source);
        let launcher = if page == "streams" {
            toolbar[11].clone()
        } else {
            toolbar[1].clone()
        };
        assert!(launcher.grab_focus());
        native_x11_key("Return");
        let dialog = focus001_dialog_after(&window, None);
        let q = focus001_button_in(&dialog, "q");
        assert!(q.grab_focus());
        for (step, key) in ["Down", "Down", "Up", "Up"].iter().enumerate() {
            let before = window.focused_widget().expect("OSK previous owner");
            assert!(TvWindow::is_within(&before, dialog.upcast_ref()));
            native_x11_key(key);
            let after = window
                .focused_widget()
                .expect("RED: native OSK vertical arrow lost focus");
            assert!(after.is_mapped() && after.is_sensitive());
            assert!(
                TvWindow::is_within(&after, dialog.upcast_ref()),
                "RED: {page} {width} OSK {key} escaped the modal at step {step}"
            );
            assert_ne!(
                after, before,
                "RED: {page} OSK vertical {key} remained stuck"
            );
        }
        assert_owner(&window, q.upcast_ref(), "OSK two Down and two Up restore q");
        native_x11_key("Escape");
        assert!(
            window.visible_dialog().is_none(),
            "OSK Escape failed to close"
        );
        window.close();
    }
    #[test]
    #[ignore = "native XTest Live OSK full vertical Down/Up compact"]
    fn gtk_focus001_recovery_osk_vertical_live_compact() {
        focus001_recovery_osk_vertical("live", 640);
    }
    #[test]
    #[ignore = "native XTest Live OSK full vertical Down/Up TV"]
    fn gtk_focus001_recovery_osk_vertical_live_tv() {
        focus001_recovery_osk_vertical("live", 1280);
    }
    #[test]
    #[ignore = "native XTest Mediathek OSK full vertical Down/Up compact"]
    fn gtk_focus001_recovery_osk_vertical_mediathek_compact() {
        focus001_recovery_osk_vertical("mediathek", 640);
    }
    #[test]
    #[ignore = "native XTest Mediathek OSK full vertical Down/Up TV"]
    fn gtk_focus001_recovery_osk_vertical_mediathek_tv() {
        focus001_recovery_osk_vertical("mediathek", 1280);
    }
    #[test]
    #[ignore = "native XTest My Channels OSK full vertical Down/Up compact"]
    fn gtk_focus001_recovery_osk_vertical_streams_compact() {
        focus001_recovery_osk_vertical("streams", 640);
    }
    #[test]
    #[ignore = "native XTest My Channels OSK full vertical Down/Up TV"]
    fn gtk_focus001_recovery_osk_vertical_streams_tv() {
        focus001_recovery_osk_vertical("streams", 1280);
    }

    fn focus001_recovery_virtual_page_batched(total: u32) {
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
        let steps = 132_u32;
        let mut current = 0_u32;
        while current < steps {
            let batch = (steps - current).min(8);
            focus001_native_repeat("Down", batch);
            current += batch;
            let owner = window.focused_widget().expect("native Down lost focus");
            assert!(
                owner.is_mapped() && owner.is_sensitive() && streams.contains_results_focus(&owner),
                "RED: batched native Down left mapped virtual GtkListView"
            );
            assert_eq!(
                streams.focus001_selected_index(),
                current,
                "RED: batched native Down lost logical row order"
            );
        }
        while current > 0 {
            let batch = current.min(8);
            focus001_native_repeat("Up", batch);
            current -= batch;
            let owner = window.focused_widget().expect("native Up lost focus");
            assert!(
                owner.is_mapped() && owner.is_sensitive() && streams.contains_results_focus(&owner),
                "RED: batched native reverse Up left mapped virtual GtkListView"
            );
            assert_eq!(
                streams.focus001_selected_index(),
                current,
                "RED: batched native reverse Up lost logical row order"
            );
        }
        assert_eq!(
            streams.remote_action_test_selected_id(),
            original,
            "RED: selected source-qualified ID changed across virtual boundary"
        );
        eprintln!("FOCUS001 recovery_virtual_native total={total} down=132 up=132 batches<=8 full_last_row_not_claimed");
        window.close();
    }

    // Each sample takes 132 physical Down and 132 Up events. This crosses
    // a 128-row virtual page boundary, but does not claim the far last row.
    #[test]
    #[ignore = "native D-pad 284 item virtual page boundary and reversal"]
    fn gtk_focus001_recovery_virtual_284_page_boundary() {
        focus001_recovery_virtual_page_batched(284);
    }
    #[test]
    #[ignore = "native D-pad 1484 item virtual page boundary and reversal"]
    fn gtk_focus001_recovery_virtual_1484_page_boundary() {
        focus001_recovery_virtual_page_batched(1_484);
    }
    #[test]
    #[ignore = "native D-pad 10000 item virtual page boundary and reversal"]
    fn gtk_focus001_recovery_virtual_10000_page_boundary() {
        focus001_recovery_virtual_page_batched(10_000);
    }
    #[test]
    #[ignore = "native D-pad 15000 item virtual page boundary and reversal"]
    fn gtk_focus001_recovery_virtual_15000_page_boundary() {
        focus001_recovery_virtual_page_batched(15_000);
    }

    // D38 F06: unlike the 132-event page-boundary sampler, this probes EVERY
    // row of the 284-row mapped ListView, then reverses every native key.
    // No model helper, synthetic scroll_to or grab_focus after initial setup.
    fn focus001_cont008_full_native_roundtrip(total: u32) {
        let (_app, window) = mapped_window("streams");
        window.set_default_size(640, 480);
        settle();
        let streams = &window.imp().streams_view;
        let search = streams.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        let id = streams.focus001_mapped_virtual_page(total, 0);
        settle();
        assert!(streams.show_and_focus_test_results());
        settle();
        assert_eq!(streams.focus001_selected_index(), 0);
        let started = std::time::Instant::now();
        let mut selected = 0_u32;
        while selected < total - 1 {
            let batch = (total - 1 - selected).min(8);
            focus001_native_repeat("Down", batch);
            selected += batch;
            let owner = window
                .focused_widget()
                .expect("RED: full native traversal lost focus");
            assert!(
                owner.is_mapped() && owner.is_sensitive() && streams.contains_results_focus(&owner),
                "RED: full native Down left mapped ListView at index {selected}"
            );
            assert_eq!(
                streams.focus001_selected_index(),
                selected,
                "RED: full native Down logical position mismatch at {selected}"
            );
            if selected >= total / 2 && selected - batch < total / 2 {
                eprintln!(
                    "FOCUS001 virtual284_full reached_mid index={selected} elapsed_ms={}",
                    started.elapsed().as_millis()
                );
            }
        }
        assert_eq!(
            streams.focus001_selected_index(),
            total - 1,
            "RED: far final row unreachable by native D-pad"
        );
        let (slots, pages, count) = streams.focus001_mapped_virtual_stats();
        assert_eq!(count, total);
        assert!(
            pages <= (total / 128 + 2) as usize && slots <= total as usize,
            "RED: unbounded virtual-page cache at final row"
        );
        while selected > 0 {
            let batch = selected.min(8);
            focus001_native_repeat("Up", batch);
            selected -= batch;
            let owner = window
                .focused_widget()
                .expect("RED: reverse traversal lost focus");
            assert!(
                owner.is_mapped() && owner.is_sensitive() && streams.contains_results_focus(&owner),
                "RED: full native Up left mapped ListView at {selected}"
            );
            assert_eq!(
                streams.focus001_selected_index(),
                selected,
                "RED: full native Up logical position mismatch at {selected}"
            );
        }
        assert_eq!(
            streams.remote_action_test_selected_id(),
            Some(id),
            "RED: source-qualified row identity changed across full native cycle"
        );
        eprintln!("FOCUS001 virtual_full total={total} native_down={} native_up={} final={} reverse=0 pages={pages} slots={slots} elapsed_ms={}", total - 1, total - 1, total - 1, started.elapsed().as_millis());
        window.close();
    }

    #[test]
    #[ignore = "native 284 row first-to-last-to-first D-pad traversal with stable ID"]
    fn gtk_focus001_cont008_virtual284_full_native_roundtrip() {
        focus001_cont008_full_native_roundtrip(284);
    }
    #[test]
    #[ignore = "native 1484 row complete first-to-middle-to-last and reverse D-pad"]
    fn gtk_focus001_cont008_virtual1484_full_native_roundtrip() {
        focus001_cont008_full_native_roundtrip(1_484);
    }

    // C09: real remote keys must cancel the OSK itself, including HomePage,
    // without navigating behind that modal or discarding the page origin.
    // No direct clicked() or grab_focus on the launcher: Search→Right→OK.
    fn focus001_cont008_osk_remote_modal(page: &str, width: i32) {
        let (_app, window) = mapped_window(page);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        if page == "live" {
            window.imp().live_view.show_toolbar_for_test();
        }
        settle();
        let controls = match page {
            "live" => window.imp().live_view.toolbar_focus_targets(),
            "mediathek" => window.imp().mediathek_view.toolbar_focus_targets(),
            _ => unreachable!(),
        };
        let search = controls[0].clone();
        let launcher = controls[1].clone();
        assert_native_input_delivery(&search);
        for key in ["XF86Back", "Escape", "XF86HomePage"] {
            assert!(search.grab_focus(), "initial mapped Search fixture");
            native_x11_key("Right");
            assert_owner(
                &window,
                &launcher,
                "native Search Right reaches OSK launcher",
            );
            native_x11_key("Return");
            let dialog = window
                .visible_dialog()
                .expect("native OK must open actual OSK modal");
            assert!(dialog.is_mapped());
            let owner = window.focused_widget().expect("OSK active focus");
            assert!(TvWindow::is_within(&owner, dialog.upcast_ref()));
            native_x11_key("Down");
            let owner = window.focused_widget().expect("OSK D-pad within modal");
            assert!(
                owner.is_mapped() && TvWindow::is_within(&owner, dialog.upcast_ref()),
                "RED: modal focus escaped before special key"
            );
            native_x11_key(key);
            settle(); // Deferred AdwDialog focus restoration must complete before checking owner.
            assert!(
                window.visible_dialog().is_none(),
                "RED: genuine special remote/escape did not dismiss OSK"
            );
            assert_eq!(
                window.imp().stack.visible_child_name().as_deref(),
                Some(page),
                "RED: OSK key navigated behind nested modal"
            );
            let owner = window.focused_widget().expect("OSK modal return focus");
            assert!(
                owner.is_mapped() && owner.is_sensitive(),
                "RED: nested OSK dismissal left invalid focus owner"
            );
            assert!(
                TvWindow::is_within(
                    &owner,
                    window.imp().stack.visible_child().unwrap().upcast_ref()
                ),
                "RED: nested OSK dismissal left its origin page"
            );
        }
        eprintln!("FOCUS001 osk_special page={page} width={width} real_XF86Back_Escape_XF86HomePage closed_modal_no_navigation=1");
        window.close();
    }
    #[test]
    #[ignore = "native Live compact OSK XF86 Back/Escape/Home modal containment"]
    fn gtk_focus001_cont008_osk_remote_live_compact() {
        focus001_cont008_osk_remote_modal("live", 640);
    }
    #[test]
    #[ignore = "native Live TV OSK XF86 Back/Escape/Home modal containment"]
    fn gtk_focus001_cont008_osk_remote_live_tv() {
        focus001_cont008_osk_remote_modal("live", 1280);
    }
    #[test]
    #[ignore = "native Mediathek compact OSK XF86 Back/Escape/Home modal containment"]
    fn gtk_focus001_cont008_osk_remote_mediathek_compact() {
        focus001_cont008_osk_remote_modal("mediathek", 640);
    }
    #[test]
    #[ignore = "native Mediathek TV OSK XF86 Back/Escape/Home modal containment"]
    fn gtk_focus001_cont008_osk_remote_mediathek_tv() {
        focus001_cont008_osk_remote_modal("mediathek", 1280);
    }

    // These probes deliberately enter the real source manager with a native
    // key sequence from the mapped My Channels search. No row action is
    // allowed to become focusable only through a test-internal clicked signal.
    fn focus001_native_source_upper_child(action: &str, width: i32) {
        let (_app, window, scratch) = focus001_manager_fixture(2);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        settle();
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert!(search.grab_focus());
        for key in ["Down", "Down", "Down", "Right", "Return", "Down", "Right"] {
            native_x11_key(key);
        }
        let manager = focus001_dialog_after(&window, None);
        let steps = match action {
            "Defaults" => 2,
            "Rename" => 3,
            _ => unreachable!(),
        };
        for _ in 0..steps {
            native_x11_key("Right");
        }
        let owner = window.focused_widget().expect("source modal opener focus");
        let opener = TvWindow::find_descendant(manager.upcast_ref(), &|node| {
            node.downcast_ref::<gtk::Button>().is_some_and(|button| {
                button.label().as_deref() == Some(action)
                    && button.is_mapped()
                    && button.is_sensitive()
                    && TvWindow::is_within(&owner, button.upcast_ref())
            })
        });
        assert!(
            opener.is_some(),
            "RED: source {action} not reachable via native Right"
        );
        native_x11_key("Return");
        let child = focus001_dialog_after(&window, Some(&manager));
        assert!(focus001_button_in(&child, "Cancel").is_mapped());
        assert!(focus001_button_in(&child, "Save").is_mapped());
        assert!(
            TvWindow::find_descendant(child.upcast_ref(), &|node| {
                node.is::<gtk::Entry>() || node.is::<adw::EntryRow>()
            })
            .is_some(),
            "RED: source {action} has no editable form"
        );
        let owner = window.focused_widget().expect("native child dialog focus");
        assert!(owner.is_mapped() && TvWindow::is_within(&owner, child.upcast_ref()));
        native_x11_key("Escape");
        assert_eq!(window.visible_dialog(), Some(manager.clone()));
        let owner = window
            .focused_widget()
            .expect("source {action} modal return focus");
        assert!(
            owner.is_mapped() && TvWindow::is_within(&owner, manager.upcast_ref()),
            "RED: Escape after {action} orphaned source-manager focus"
        );
        native_x11_key("Escape");
        assert!(window.visible_dialog().is_none());
        eprintln!("FOCUS001 source_upper_child native_action={action} viewport={width} modal_escape_return=1");
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove synthetic manager fixture");
    }

    #[test]
    #[ignore = "native source Defaults portal, edit form, modal Escape/return compact"]
    fn gtk_focus001_xtest_source_defaults_nested_compact() {
        focus001_native_source_upper_child("Defaults", 640);
    }
    #[test]
    #[ignore = "native source Defaults portal, edit form, modal Escape/return TV"]
    fn gtk_focus001_xtest_source_defaults_nested_tv() {
        focus001_native_source_upper_child("Defaults", 1280);
    }
    #[test]
    #[ignore = "native source Rename portal, edit form, modal Escape/return compact"]
    fn gtk_focus001_xtest_source_rename_nested_compact() {
        focus001_native_source_upper_child("Rename", 640);
    }
    #[test]
    #[ignore = "native source Rename portal, edit form, modal Escape/return TV"]
    fn gtk_focus001_xtest_source_rename_nested_tv() {
        focus001_native_source_upper_child("Rename", 1280);
    }

    // The source action grid has an approved two-line spatial contract. Enter
    // a specific lower action strictly through arrows from the real upper
    // switch; an unreachable control is an actual RED, not a skipped test.
    fn focus001_native_source_lower_action(action: &str) {
        let (_app, window, scratch) = focus001_manager_fixture(3);
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert!(search.grab_focus());
        for key in [
            "Down", "Down", "Down", "Right", "Return", "Down", "Right", "Right",
        ] {
            native_x11_key(key);
        }
        let manager = focus001_dialog_after(&window, None);
        let upper = window.focused_widget().expect("native upper Refresh");
        assert!(
            TvWindow::find_descendant(manager.upcast_ref(), &|node| {
                node.downcast_ref::<gtk::Button>().is_some_and(|b| {
                    b.label().as_deref() == Some("Refresh")
                        && TvWindow::is_within(&upper, b.upcast_ref())
                })
            })
            .is_some(),
            "RED: first source Refresh not native reachable"
        );
        native_x11_key("Down");
        let mut reached = false;
        for _ in 0..5 {
            let owner = window.focused_widget().expect("source lower route focus");
            reached |= TvWindow::find_descendant(manager.upcast_ref(), &|node| {
                node.downcast_ref::<gtk::Button>().is_some_and(|b| {
                    b.label().as_deref() == Some(action)
                        && b.is_mapped()
                        && b.is_sensitive()
                        && TvWindow::is_within(&owner, b.upcast_ref())
                })
            })
            .is_some();
            if reached {
                break;
            }
            native_x11_key("Right");
        }
        assert!(
            reached,
            "RED: native upper-to-lower source grid never reaches {action}"
        );
        // Do not execute Replace, Delete or reordering inadvertently here.
        // The path's actual enabled action is the result being audited.
        eprintln!("FOCUS001 source_lower_action reached={action} XTest_Down_Right=1");
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove synthetic source fixture");
    }
    #[test]
    #[ignore = "native action source Replace through two-line source-manager grid"]
    fn gtk_focus001_xtest_source_lower_replace_native_path() {
        focus001_native_source_lower_action("Replace");
    }
    #[test]
    #[ignore = "native action source Delete through two-line source-manager grid"]
    fn gtk_focus001_xtest_source_lower_delete_native_path() {
        focus001_native_source_lower_action("Delete");
    }
    #[test]
    #[ignore = "native action source Down reorder through two-line source-manager grid"]
    fn gtk_focus001_xtest_source_lower_reorder_down_native_path() {
        focus001_native_source_lower_action("↓");
    }

    // A real guide with a source-qualified default must explicitly confirm:
    // cancelling cannot mutate the SQLite singleton; confirmation can.
    #[test]
    #[ignore = "native guide default confirm and cancel on disposable 3-guide SQLite"]
    fn gtk_focus001_xtest_guide_shared_default_confirmation_persists() {
        crate::epg::desktop::focus001_seed_disposable_guides(3);
        let (_app, window) = mapped_window("streams");
        assert_native_input_delivery(&window.imp().streams_view.toolbar_focus_targets()[0]);
        crate::epg::desktop::show_guide_manager(window.upcast_ref::<gtk::Window>());
        let manager = focus001_dialog_after(&window, None);
        let action = focus001_button_in(&manager, "Use as Shared Default");
        assert!(action.grab_focus());
        native_x11_key("Right");
        let owner = window.focused_widget().expect("guide Refresh edge");
        assert!(TvWindow::find_descendant(manager.upcast_ref(), &|node| {
            node.downcast_ref::<gtk::Button>().is_some_and(|b| {
                b.label().as_deref() == Some("Refresh")
                    && TvWindow::is_within(&owner, b.upcast_ref())
            })
        })
        .is_some());
        native_x11_key("Left");
        assert_owner(&window, action.upcast_ref(), "guide Refresh Left default");
        let store = crate::streams::storage::Storage::new();
        let conn = store.connect().expect("isolated guide DB");
        let selected_before: Option<String> = conn
            .query_row(
                "SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .expect("previous default");
        assert!(selected_before.is_none());
        native_x11_key("Return");
        let confirm = focus001_dialog_after(&window, Some(&manager));
        let cancel = focus001_button_in(&confirm, "Cancel");
        assert!(cancel.grab_focus());
        native_x11_key("Return");
        assert_eq!(window.visible_dialog(), Some(manager.clone()));
        let unchanged: Option<String> = conn
            .query_row(
                "SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .expect("cancel unchanged");
        assert!(unchanged.is_none(), "RED: Cancel mutated shared default");
        assert!(action.grab_focus());
        native_x11_key("Return");
        let confirm = focus001_dialog_after(&window, Some(&manager));
        let approve = focus001_button_in(&confirm, "Confirm Shared Default");
        assert!(approve.grab_focus());
        native_x11_key("Return");
        let mut selected = None;
        for _ in 0..60 {
            settle();
            selected = conn
                .query_row(
                    "SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1",
                    [],
                    |row| row.get::<_, Option<String>>(0),
                )
                .expect("persisted shared default");
            if selected.is_some() {
                break;
            }
        }
        assert!(
            selected.is_some(),
            "RED: native confirmation did not persist shared guide"
        );
        assert_eq!(
            window.visible_dialog(),
            Some(manager.clone()),
            "RED: successful confirmation did not return to manager"
        );
        let owner = window
            .focused_widget()
            .expect("guide manager after confirm");
        assert!(owner.is_mapped() && TvWindow::is_within(&owner, manager.upcast_ref()));
        eprintln!(
            "FOCUS001 guide_shared_default native_cancel=1 native_confirm=1 saved={selected:?}"
        );
        native_x11_key("Escape");
        window.close();
    }

    #[test]
    #[ignore = "guide Remove confirmation keeps 3 synthetic guides on native Cancel"]
    fn gtk_focus001_xtest_guide_remove_keep_native_roundtrip() {
        crate::epg::desktop::focus001_seed_disposable_guides(3);
        let (_app, window) = mapped_window("streams");
        assert_native_input_delivery(&window.imp().streams_view.toolbar_focus_targets()[0]);
        crate::epg::desktop::show_guide_manager(window.upcast_ref::<gtk::Window>());
        let manager = focus001_dialog_after(&window, None);
        let default = focus001_button_in(&manager, "Use as Shared Default");
        assert!(default.grab_focus());
        for _ in 0..3 {
            native_x11_key("Right");
        }
        let owner = window.focused_widget().expect("Remove native owner");
        assert!(
            TvWindow::find_descendant(manager.upcast_ref(), &|node| {
                node.downcast_ref::<gtk::Button>().is_some_and(|b| {
                    b.label().as_deref() == Some("Remove")
                        && TvWindow::is_within(&owner, b.upcast_ref())
                })
            })
            .is_some(),
            "RED: guide Remove action not D-pad reachable"
        );
        native_x11_key("Return");
        let child = focus001_dialog_after(&window, Some(&manager));
        let keep = focus001_button_in(&child, "Keep Guide");
        assert!(keep.grab_focus());
        native_x11_key("Return");
        assert_eq!(window.visible_dialog(), Some(manager.clone()));
        let store = crate::streams::storage::Storage::new();
        let conn = store.connect().expect("disposable guide DB");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM epg_sources", [], |row| row.get(0))
            .expect("guide rows after Keep");
        assert_eq!(count, 3, "RED: cancelled guide removal erased guide");
        let owner = window.focused_widget().expect("guide Remove return owner");
        assert!(owner.is_mapped() && TvWindow::is_within(&owner, manager.upcast_ref()));
        eprintln!(
            "FOCUS001 guide_remove_keep native_right=3 native_modal_cancel=1 retained={count}"
        );
        native_x11_key("Escape");
        window.close();
    }

    // Native Home and Escape are separate, with a mapped modal keeping the
    // window's focus from falling through to the page or global switcher.
    #[test]
    #[ignore = "source modal Home must remain contained; Escape closes and restores page focus"]
    fn gtk_focus001_xtest_source_manager_home_escape_semantics() {
        let (_app, window, scratch) = focus001_manager_fixture(1);
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert!(search.grab_focus());
        for key in ["Down", "Down", "Down", "Right", "Return"] {
            native_x11_key(key);
        }
        let manager = focus001_dialog_after(&window, None);
        native_x11_key("Home");
        assert_eq!(
            window.visible_dialog(),
            Some(manager.clone()),
            "RED: Home improperly dismissed source modal"
        );
        let owner = window.focused_widget().expect("Home modal owner");
        assert!(
            owner.is_mapped() && TvWindow::is_within(&owner, manager.upcast_ref()),
            "RED: Home escapes source modal focus"
        );
        native_x11_key("Escape");
        assert!(
            window.visible_dialog().is_none(),
            "Escape must dismiss source manager"
        );
        let restored = window.focused_widget().expect("Escape restored owner");
        assert!(
            restored.is_mapped()
                && TvWindow::is_within(&restored, window.imp().streams_view.upcast_ref())
        );
        eprintln!("FOCUS001 source modal Home/Escape independently delivered via XTest");
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove disposable fixture");
    }

    #[test]
    #[ignore = "guide modal Home versus Escape, and retained mapped My Channels return"]
    fn gtk_focus001_xtest_guide_manager_home_escape_semantics() {
        crate::epg::desktop::focus001_seed_disposable_guides(3);
        let (_app, window) = mapped_window("streams");
        assert_native_input_delivery(&window.imp().streams_view.toolbar_focus_targets()[0]);
        crate::epg::desktop::show_guide_manager(window.upcast_ref::<gtk::Window>());
        let manager = focus001_dialog_after(&window, None);
        native_x11_key("Home");
        assert_eq!(window.visible_dialog(), Some(manager.clone()));
        let owner = window.focused_widget().expect("guide Home owner");
        assert!(
            owner.is_mapped() && TvWindow::is_within(&owner, manager.upcast_ref()),
            "RED: native Home escaped guide modal"
        );
        native_x11_key("Escape");
        assert!(window.visible_dialog().is_none());
        let owner = window.focused_widget().expect("guide Escape return owner");
        assert!(
            owner.is_mapped()
                && TvWindow::is_within(&owner, window.imp().streams_view.upcast_ref()),
            "RED: guide Escape did not restore origin page"
        );
        eprintln!("FOCUS001 guide manager native Home/Escape distinct; origin retained");
        window.close();
    }

    // Full row-by-row mapped OSK arrow survey: each row receives exactly one
    // initial seed after a *native Search->Keyboard->Return* opener. Every
    // horizontal cell transition and wrap is a genuine XTest key event.
    // This does not claim to cover all vertical-entry paths or pixels.
    fn focus001_native_osk_complete_rows(width: i32, symbol_page: bool) {
        let (_app, window) = mapped_window("mediathek");
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        settle();
        let toolbar = window.imp().mediathek_view.toolbar_focus_targets();
        assert_native_input_delivery(&toolbar[0]);
        assert!(toolbar[0].grab_focus());
        native_x11_key("Right");
        assert_owner(&window, &toolbar[1], "native Search Right Keyboard");
        native_x11_key("Return");
        let dialog = focus001_dialog_after(&window, None);
        let key = |label: &str| -> gtk::Button {
            TvWindow::find_descendant(dialog.upcast_ref(), &|w| {
                w.downcast_ref::<gtk::Button>().is_some_and(|button| {
                    button.label().as_deref() == Some(label) && button.is_mapped()
                })
            })
            .unwrap_or_else(|| panic!("real OSK grid key absent: {label}"))
            .downcast::<gtk::Button>()
            .expect("native GTK OSK button")
        };
        if symbol_page {
            assert!(key("Symbols").grab_focus());
            native_x11_key("Return");
            assert!(
                key("Letters").is_mapped(),
                "native Symbols key did not change layout"
            );
        }
        let rows: Vec<Vec<&str>> = if symbol_page {
            vec![
                vec!["!", "'", "\"", "(", ")", "[", "]", "{", "}", "\\", "|"],
                vec![";", "*", "$", "^", "<", ">", "€", "£", "§", "°", "~"],
                vec!["ä", "ö", "ü", "ß", "ẞ", ":", "/", "?", "&", "="],
            ]
        } else {
            vec![
                vec!["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"],
                vec!["q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "ü"],
                vec!["a", "s", "d", "f", "g", "h", "j", "k", "l", "ö", "ä"],
                vec!["z", "x", "c", "v", "b", "n", "m", "ß", ",", "."],
                vec![":", "/", "?", "&", "=", "#", "@", "+", "-", "_", "%", "~"],
            ]
        };
        let mut edges = 0usize;
        for row in &rows {
            let first = key(row[0]);
            assert!(first.grab_focus(), "OSK row entry not focusable");
            for next in row.iter().skip(1) {
                native_x11_key("Right");
                assert_owner(
                    &window,
                    key(next).upcast_ref(),
                    "native OSK horizontal row walk",
                );
                edges += 1;
            }
            native_x11_key("Right");
            assert_owner(&window, first.upcast_ref(), "native OSK row wrap");
            native_x11_key("Left");
            assert_owner(
                &window,
                key(row[row.len() - 1]).upcast_ref(),
                "native OSK reverse wrap",
            );
            edges += 2;
        }
        eprintln!("FOCUS001 osk_full_horizontal viewport={width} symbols={symbol_page} rows={} native_edges={edges}", rows.len());
        native_x11_key("Escape");
        assert!(window.visible_dialog().is_none());
        window.close();
    }

    #[test]
    #[ignore = "all 5 OSK numeric/letter rows horizontal native XTest 640x480"]
    fn gtk_focus001_xtest_osk_alphanumeric_complete_rows_compact() {
        focus001_native_osk_complete_rows(640, false);
    }
    #[test]
    #[ignore = "all 5 OSK numeric/letter rows horizontal native XTest 1280x800"]
    fn gtk_focus001_xtest_osk_alphanumeric_complete_rows_tv() {
        focus001_native_osk_complete_rows(1280, false);
    }
    #[test]
    #[ignore = "all 3 OSK symbol rows horizontal native XTest 640x480"]
    fn gtk_focus001_xtest_osk_symbols_complete_rows_compact() {
        focus001_native_osk_complete_rows(640, true);
    }
    #[test]
    #[ignore = "all 3 OSK symbol rows horizontal native XTest 1280x800"]
    fn gtk_focus001_xtest_osk_symbols_complete_rows_tv() {
        focus001_native_osk_complete_rows(1280, true);
    }

    fn focus001_native_streams_card_actions(enter: &str) {
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        let search = streams.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        let ids = streams.install_remote_action_test_rows();
        settle();
        assert!(
            streams.show_and_focus_test_results(),
            "mapped synthetic card absent"
        );
        let origin = streams.remote_action_test_selected_id();
        assert_eq!(origin.as_deref(), Some(ids[0].as_str()));
        native_x11_key(enter);
        let actions = streams
            .remote_action_test_buttons()
            .expect("native card expanded");
        assert_eq!(
            actions.len(),
            3,
            "Play Edit Mapping must exist on expanded row"
        );
        let play = &actions[0];
        assert!(play.is_mapped() && play.is_sensitive());
        assert_owner(
            &window,
            play.upcast_ref(),
            "native entry focuses immediate Play",
        );
        for (i, key) in [(1usize, "Down"), (2, "Down"), (1, "Up"), (0, "Up")] {
            native_x11_key(key);
            assert_owner(
                &window,
                actions[i].upcast_ref(),
                "native expanded card Play/Edit/Mapping reversible navigation",
            );
        }
        assert_eq!(
            streams.remote_action_test_counts(),
            (0, 0, 0),
            "navigation must never activate playback, editing or mapping"
        );
        native_x11_key("Escape");
        assert_eq!(
            streams.remote_action_test_selected_id(),
            origin,
            "native card Escape changed selected logical identity"
        );
        let owner = window
            .focused_widget()
            .expect("native Escape to card owner");
        assert!(
            streams.contains_results_focus(&owner) && owner.is_mapped(),
            "RED: expanded card Escape orphaned focus or jumped to toolbar"
        );
        eprintln!(
            "FOCUS001 streams_card native_enter={enter} native_actions=3 reverse=4 escaped=1"
        );
        window.close();
    }
    #[test]
    #[ignore = "native My Channels first OK opens Play and full three-action reversal"]
    fn gtk_focus001_xtest_streams_card_full_action_graph_ok() {
        focus001_native_streams_card_actions("Return");
    }
    #[test]
    #[ignore = "native My Channels Right opens Play and full three-action reversal"]
    fn gtk_focus001_xtest_streams_card_full_action_graph_right() {
        focus001_native_streams_card_actions("Right");
    }

    // The actual source facet must be focusable with a populated disposable
    // source registry and a native dropdown selection. Each case has no URL
    // refresh or playlist import and cannot mutate an owner profile.
    #[test]
    #[ignore = "native populated My Channels source-facet select and Clear Filters return"]
    fn gtk_focus001_xtest_streams_source_selector_populated_native_clear() {
        let (_app, window, scratch) = focus001_manager_fixture(3);
        let controls = window.imp().streams_view.toolbar_focus_targets();
        let dropdown = controls[1]
            .clone()
            .downcast::<gtk::DropDown>()
            .expect("real source facet");
        for _ in 0..60 {
            settle();
            if dropdown.model().is_some_and(|m| m.n_items() >= 4) {
                break;
            }
        }
        assert!(
            dropdown.model().is_some_and(|model| model.n_items() >= 4),
            "RED: all three disposable sources must be available to the source facet"
        );
        assert_eq!(dropdown.selected(), 0);
        assert!(dropdown.grab_focus());
        native_x11_key("space");
        native_x11_key("Down");
        native_x11_key("Return");
        for _ in 0..45 {
            settle();
            if dropdown.selected() != 0 {
                break;
            }
        }
        assert_ne!(
            dropdown.selected(),
            0,
            "RED: remote cannot select a populated source from native dropdown"
        );
        let clear = controls[7]
            .clone()
            .downcast::<gtk::Button>()
            .expect("Clear Filters");
        assert!(
            clear.is_mapped() && clear.is_sensitive(),
            "native facet clear not enabled"
        );
        assert!(clear.grab_focus());
        native_x11_key("Return");
        for _ in 0..40 {
            settle();
            if dropdown.selected() == 0 {
                break;
            }
        }
        assert_eq!(
            dropdown.selected(),
            0,
            "RED: Clear Filters did not restore all sources"
        );
        eprintln!(
            "FOCUS001 populated_source_facet native_open_select=1 clear_filters=1 SQLite_sources=3"
        );
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove disposable source filter fixture");
    }

    // Four genuine persisted facet dropdowns (not fabricated labels):
    // native Space, Down, Return, Clear and unchanged unrelated source data.
    fn focus001_rich_facet_native(facet_index: usize, width: i32) {
        let (_app, window) = mapped_window("streams");
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let scratch =
            std::env::temp_dir().join(format!("focus001-rich-facet-{}", uuid::Uuid::new_v4()));
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&scratch)
            .expect("disposable private facet profile");
        let file = scratch.join("facets.sqlite");
        crate::streams::view::TvStreamsView::focus001_seed_rich_facet_sources(file.clone());
        let view = &window.imp().streams_view;
        let ready = view.focus001_attach_disposable_service(file);
        for _ in 0..75 {
            settle();
            if ready.get() {
                break;
            }
        }
        assert!(ready.get(), "rich facet list failed to load");
        let toolbar = view.toolbar_focus_targets();
        assert_native_input_delivery(&toolbar[0]);
        let dropdown = toolbar[facet_index]
            .clone()
            .downcast::<gtk::DropDown>()
            .expect("actual enabled facet");
        for _ in 0..60 {
            settle();
            if dropdown.model().is_some_and(|m| m.n_items() >= 3) {
                break;
            }
        }
        assert!(dropdown.is_mapped() && dropdown.is_sensitive());
        assert!(
            dropdown.model().is_some_and(|m| m.n_items() >= 3),
            "RED: rich persisted metadata absent from native facet"
        );
        assert_eq!(dropdown.selected(), 0);
        assert!(dropdown.grab_focus());
        native_x11_key("space");
        native_x11_key("Down");
        native_x11_key("Return");
        for _ in 0..35 {
            settle();
            if dropdown.selected() > 0 {
                break;
            }
        }
        assert!(
            dropdown.selected() > 0,
            "RED: native Space/Down/Return could not select rich facet"
        );
        let clear = toolbar[7]
            .clone()
            .downcast::<gtk::Button>()
            .expect("Clear Filters");
        assert!(clear.is_mapped() && clear.is_sensitive());
        assert!(clear.grab_focus());
        native_x11_key("Return");
        for _ in 0..35 {
            settle();
            if dropdown.selected() == 0 {
                break;
            }
        }
        assert_eq!(
            dropdown.selected(),
            0,
            "RED: Clear Filters must reset selected persisted facet"
        );
        eprintln!("FOCUS001 RICH_FACET facet={facet_index} width={width} native_select=1 native_clear=1 channels=12");
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove disposable rich fixture");
    }
    #[test]
    #[ignore = "XTest rich persisted group facet and native Clear Filters"]
    fn gtk_focus001_recovery_rich_group_compact() {
        focus001_rich_facet_native(2, 640);
    }
    #[test]
    #[ignore = "XTest rich persisted group facet and native Clear Filters"]
    fn gtk_focus001_recovery_rich_group_tv() {
        focus001_rich_facet_native(2, 1280);
    }
    #[test]
    #[ignore = "XTest rich persisted language facet and native Clear Filters"]
    fn gtk_focus001_recovery_rich_language_compact() {
        focus001_rich_facet_native(4, 640);
    }
    #[test]
    #[ignore = "XTest rich persisted language facet and native Clear Filters"]
    fn gtk_focus001_recovery_rich_language_tv() {
        focus001_rich_facet_native(4, 1280);
    }
    #[test]
    #[ignore = "XTest rich persisted country facet and native Clear Filters"]
    fn gtk_focus001_recovery_rich_country_compact() {
        focus001_rich_facet_native(5, 640);
    }
    #[test]
    #[ignore = "XTest rich persisted country facet and native Clear Filters"]
    fn gtk_focus001_recovery_rich_country_tv() {
        focus001_rich_facet_native(5, 1280);
    }
    #[test]
    #[ignore = "XTest rich persisted region facet and native Clear Filters"]
    fn gtk_focus001_recovery_rich_region_compact() {
        focus001_rich_facet_native(6, 640);
    }
    #[test]
    #[ignore = "XTest rich persisted region facet and native Clear Filters"]
    fn gtk_focus001_recovery_rich_region_tv() {
        focus001_rich_facet_native(6, 1280);
    }

    // Real Mediathek cards, bound to synthetic ShowObject items, are installed
    // on the actual app ListBox. The only injected input remains the XTest
    // arrows/Return; no Mediathek API, link or playback action is triggered.
    fn focus001_mediathek_loaded_graph(width: i32) {
        let (_app, window) = mapped_window("streams");
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        assert_native_input_delivery(&window.imp().streams_view.toolbar_focus_targets()[0]);
        window.imp().stack.set_visible_child_name("mediathek");
        settle();
        let med = &window.imp().mediathek_view;
        med.focus001_install_disposable_results(5, true);
        settle();
        let controls = med.toolbar_focus_targets();
        let list = med.focus001_result_list();
        let first = list
            .row_at_index(0)
            .expect("synthetic real Mediathek first card");
        let last = list
            .row_at_index(4)
            .expect("synthetic real Mediathek last card");
        assert!(first.is_mapped() && last.is_mapped());
        assert!(controls[0].grab_focus());
        // The exact existing Down chain is assessed here rather than using
        // internal focus_vertical_navigation() without GDK propagation.
        let mut positions = Vec::<String>::new();
        let mut reached = false;
        for _ in 0..9 {
            native_x11_key("Down");
            let focus = window
                .focused_widget()
                .expect("native Mediathek Down focus");
            assert!(focus.is_mapped() && focus.is_sensitive());
            positions.push(focus.type_().name().to_string());
            if TvWindow::is_within(&focus, first.upcast_ref()) {
                reached = true;
                break;
            }
        }
        eprintln!("FOCUS001 med_loaded viewport={width} native_down_chain={positions:?} reached_first={reached}");
        assert!(
            reached,
            "RED: native Down route cannot reach loaded Mediathek result card from Search"
        );
        for index in 1..5 {
            native_x11_key("Down");
            let row = list.row_at_index(index).expect("next actual result row");
            assert_owner(
                &window,
                row.upcast_ref(),
                "loaded Mediathek next row native Down",
            );
        }
        assert_owner(&window, last.upcast_ref(), "loaded Mediathek last card");
        for index in (0..4).rev() {
            native_x11_key("Up");
            let row = list
                .row_at_index(index)
                .expect("previous actual result row");
            assert_owner(
                &window,
                row.upcast_ref(),
                "loaded Mediathek Up reverses row",
            );
        }
        // One more Up must leave the list through the actual sort control.
        native_x11_key("Up");
        assert_owner(&window, &controls[6], "Mediathek results Up reaches sort");
        eprintln!(
            "FOCUS001 med_loaded viewport={width} total_rows=5 native_reverse=4 sort_return=1"
        );
        window.close();
    }
    #[test]
    #[ignore = "native loaded Mediathek search/filters/5 cards and Up reverse compact"]
    fn gtk_focus001_xtest_mediathek_loaded_results_compact() {
        focus001_mediathek_loaded_graph(640);
    }
    #[test]
    #[ignore = "native loaded Mediathek search/filters/5 cards and Up reverse TV"]
    fn gtk_focus001_xtest_mediathek_loaded_results_tv() {
        focus001_mediathek_loaded_graph(1280);
    }

    // Continuation-012 D10: compare directional Up with SEMANTIC remote Back
    // from the *same populated first result row*. A hardcoded model-only route
    // cannot prove what GTK's mapped controller actually does.
    fn focus001_cont012_mediathek_back_vs_up(width: i32) {
        let (_app, window) = mapped_window("streams");
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        assert_native_input_delivery(&window.imp().streams_view.toolbar_focus_targets()[0]);
        window.imp().stack.set_visible_child_name("mediathek");
        settle();
        let med = &window.imp().mediathek_view;
        med.focus001_install_disposable_results(5, true);
        settle();
        let controls = med.toolbar_focus_targets();
        let broadcaster = &controls[controls.len() - 3];
        let duration = &controls[controls.len() - 2];
        let sort = &controls[controls.len() - 1];
        assert!(broadcaster.is_mapped() && duration.is_mapped() && sort.is_mapped());
        let first = med
            .focus001_result_list()
            .row_at_index(0)
            .expect("disposable first actual Mediathek result");
        assert!(first.is_mapped() && first.is_sensitive());
        assert!(
            controls[0].grab_focus(),
            "genuine mapped Mediathek Search origin"
        );
        assert_owner(&window, &controls[0], "populated Mediathek Search entry");
        let mut observed = Vec::<String>::new();
        let mut reached = false;
        for _ in 0..12 {
            native_x11_key("Down");
            let owner = window
                .focused_widget()
                .expect("native Mediathek Down orphaned focus");
            assert!(owner.is_mapped() && owner.is_sensitive());
            if let Some(index) = controls
                .iter()
                .position(|control| TvWindow::is_within(&owner, control))
            {
                observed.push(format!("toolbar-{index}"));
            } else if TvWindow::is_within(&owner, first.upcast_ref()) {
                observed.push("result-first".to_owned());
                reached = true;
                break;
            } else {
                observed.push(format!("other-{}", owner.type_().name()));
            }
        }
        eprintln!("FOCUS001 cont012 D10 viewport={width} connected_down={observed:?} first_reached={reached}");
        assert!(
            reached,
            "RED: populated first Mediathek result unreachable from Search via native Down"
        );
        assert_owner(
            &window,
            first.upcast_ref(),
            "D10 native first result from Search",
        );
        native_x11_key("Up");
        assert_owner(&window, sort, "D10 physical Up from result must reach Sort");
        native_x11_key("Down");
        assert_owner(
            &window,
            first.upcast_ref(),
            "D10 native Down locally reverses Sort to result",
        );
        native_x11_key("XF86Back");
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("mediathek"),
            "RED: Back from Mediathek result switched global page"
        );
        assert_owner(
            &window,
            broadcaster,
            "D10 semantic Back must return to broadcaster browse portal, not pretend to be Up",
        );
        assert!(
            broadcaster != sort,
            "D10 directional Up and semantic Back must have distinct intentions"
        );
        eprintln!("FOCUS001 cont012 D10 viewport={width} directional_up=Sort semantic_back=Broadcaster retained_page=mediathek mapped=1");
        window.close();
    }

    #[test]
    #[ignore = "native populated Mediathek first result Up vs Back difference compact"]
    fn gtk_focus001_cont012_mediathek_populated_back_vs_up_compact() {
        focus001_cont012_mediathek_back_vs_up(640);
    }
    #[test]
    #[ignore = "native populated Mediathek first result Up vs Back difference TV"]
    fn gtk_focus001_cont012_mediathek_populated_back_vs_up_tv() {
        focus001_cont012_mediathek_back_vs_up(1280);
    }

    // The four *actual* primary and secondary Mediathek actions form one
    // connected GTK focus row. Native XTest events discover and replay its
    // whole horizontal graph after an actual row Down/OK expansion.
    fn focus001_mediathek_secondary_actions(width: i32) {
        let (_app, window) = mapped_window("streams");
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        assert_native_input_delivery(&window.imp().streams_view.toolbar_focus_targets()[0]);
        window.imp().stack.set_visible_child_name("mediathek");
        settle();
        let med = &window.imp().mediathek_view;
        med.focus001_install_disposable_results(5, false);
        settle();
        let list = med.focus001_result_list();
        let first = list.row_at_index(0).expect("native first result row");
        assert!(first.grab_focus());
        for _ in 0..2 {
            native_x11_key("Down");
        }
        let row = list.row_at_index(2).expect("middle mapped Mediathek card");
        assert_owner(&window, row.upcast_ref(), "real native middle-card entry");
        native_x11_key("Return");
        let labels = ["Play", "Copy Video URL", "Download", "Open Website"];
        let controls: Vec<gtk::Widget> = labels
            .iter()
            .map(|label| {
                TvWindow::find_descendant(row.upcast_ref(), &|w| {
                    w.tooltip_text().as_deref() == Some(*label) && w.is_mapped() && w.is_sensitive()
                })
                .unwrap_or_else(|| panic!("real mapped action absent: {label}"))
            })
            .collect();
        assert_owner(&window, &controls[0], "native OK must enter primary Play");
        let mut edges = vec![Vec::<(usize, &'static str)>::new(); controls.len()];
        let keys = ["Left", "Right", "Up", "Down"];
        for i in 0..controls.len() {
            for key in keys {
                assert!(controls[i].grab_focus(), "actual action not focusable");
                native_x11_key(key);
                let focused = window
                    .focused_widget()
                    .expect("focused action or neighbour");
                if let Some(j) = controls
                    .iter()
                    .position(|w| TvWindow::is_within(&focused, w))
                {
                    if j != i {
                        edges[i].push((j, key));
                    }
                }
            }
        }
        let mut found = vec![false; controls.len()];
        let mut parent = vec![None::<(usize, &'static str)>; controls.len()];
        let mut queue = std::collections::VecDeque::from([0usize]);
        found[0] = true;
        while let Some(i) = queue.pop_front() {
            for &(j, key) in &edges[i] {
                if !found[j] {
                    found[j] = true;
                    parent[j] = Some((i, key));
                    queue.push_back(j);
                }
            }
        }
        eprintln!("FOCUS001 MED_ACTION_GRAPH width={width} reachable={found:?} edges={edges:?}");
        assert!(
            found.iter().all(|yes| *yes),
            "RED: real four-action Mediathek card lacks complete native focus reachability"
        );
        for destination in 1..controls.len() {
            assert!(controls[0].grab_focus());
            let mut steps = Vec::<(usize, &'static str)>::new();
            let mut pos = destination;
            while pos != 0 {
                let (previous, key) = parent[pos].expect("real focus graph path");
                steps.push((pos, key));
                pos = previous;
            }
            for (next, key) in steps.into_iter().rev() {
                native_x11_key(key);
                assert_owner(
                    &window,
                    &controls[next],
                    "native replay Mediathek secondary action",
                );
            }
        }
        native_x11_key("Escape");
        let owner = window
            .focused_widget()
            .expect("native escape Mediathek card");
        assert!(
            TvWindow::is_within(&owner, row.upcast_ref()) && owner.is_mapped(),
            "RED: secondary action Escape lost original card identity"
        );
        window.close();
    }
    #[test]
    #[ignore = "real Mediathek 4-action native focus graph at compact size"]
    fn gtk_focus001_recovery_mediathek_actions_compact() {
        focus001_mediathek_secondary_actions(640);
    }
    #[test]
    #[ignore = "real Mediathek 4-action native focus graph at TV size"]
    fn gtk_focus001_recovery_mediathek_actions_tv() {
        focus001_mediathek_secondary_actions(1280);
    }

    // A separate row-local test isolates the real expanded primary Play
    // portal from any existing top-toolbar Down bug, using only one row seed
    // followed by genuine remote arrows and OK. It never activates a URL.
    #[test]
    #[ignore = "native real Mediathek middle-card OK expands into Play without playback"]
    fn gtk_focus001_xtest_mediathek_middle_card_ok_primary_play() {
        let (_app, window) = mapped_window("streams");
        assert_native_input_delivery(&window.imp().streams_view.toolbar_focus_targets()[0]);
        window.imp().stack.set_visible_child_name("mediathek");
        settle();
        let med = &window.imp().mediathek_view;
        med.focus001_install_disposable_results(5, false);
        settle();
        let list = med.focus001_result_list();
        let first = list.row_at_index(0).expect("first real card");
        assert!(first.grab_focus());
        native_x11_key("Down");
        native_x11_key("Down");
        let middle = list.row_at_index(2).expect("middle real card");
        assert_owner(&window, middle.upcast_ref(), "native first -> third card");
        native_x11_key("Return");
        let split = TvWindow::find_descendant(middle.upcast_ref(), &|w| {
            w.is::<adw::SplitButton>() && w.is_mapped() && w.is_sensitive()
        })
        .expect("expanded Mediathek primary Play SplitButton");
        assert_owner(&window, &split, "native card OK must put focus on Play");
        native_x11_key("Escape");
        let owner = window.focused_widget().expect("card Escape focus");
        assert!(
            owner.is_mapped() && TvWindow::is_within(&owner, middle.upcast_ref()),
            "RED: Escape from expanded Mediathek card lost logical row owner"
        );
        eprintln!("FOCUS001 mediathek_native middle_card_2_down=1 play_entry=1 escape=1");
        window.close();
    }

    #[test]
    #[ignore = "real XTest source card to programme picker without direct mapping focus seeding"]
    fn gtk_focus001_xtest_mapping_card_native_portal_no_seeding() {
        let (row_id, source_id, _guide_id) = focus001_mapping_fixture(1, 1);
        let (_app, window) = mapped_window("streams");
        let streams = &window.imp().streams_view;
        assert_native_input_delivery(&streams.toolbar_focus_targets()[0]);
        streams.focus001_install_persisted_mapping_row(&row_id, &source_id);
        settle();
        assert!(streams.show_and_focus_test_results());
        native_x11_key("Return");
        settle();
        let buttons = streams
            .remote_action_test_buttons()
            .expect("real expanded card actions");
        let mapping = &buttons[2];
        assert!(mapping.is_mapped() && mapping.is_sensitive());
        // Unlike earlier picker tests, no grab_focus on the mapping action.
        // The portal must be reachable by ordinary remote spatial keys.
        let mut owners = Vec::<String>::new();
        let mut reached = false;
        for _ in 0..10 {
            if let Some(owner) = window.focused_widget() {
                if TvWindow::is_within(&owner, mapping.upcast_ref()) {
                    reached = true;
                    break;
                }
                owners.push(owner.type_().name().to_string());
            }
            native_x11_key("Right");
        }
        eprintln!("FOCUS001 mapping_no_seeding arrow_owners={owners:?} mapping_reached={reached}");
        assert!(reached, "RED: programme picker is not natively reachable from source card without direct focus seeding");
        native_x11_key("Return");
        assert!(focus001_dialog_after(&window, None).is_mapped());
        native_x11_key("Escape");
        assert!(window.visible_dialog().is_none());
        window.close();
    }
    // The keyboard Home keysym is NOT the remote XF86HomePage keysym.
    // The older source/guide tests exercised the former and cannot establish
    // remote semantics. This sends the actual remote keys through XTest.
    #[test]
    #[ignore = "connected source-manager actual XF86 Back and Home semantics"]
    fn gtk_focus001_xtest_source_manager_actual_remote_back_and_home() {
        let (_app, window, scratch) = focus001_manager_fixture(1);
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        let open = || {
            assert!(search.grab_focus());
            for key in ["Down", "Down", "Down", "Right", "Return"] {
                native_x11_key(key);
            }
            focus001_dialog_after(&window, None)
        };
        let manager = open();
        assert!(manager.is_mapped());
        native_x11_key("XF86Back");
        assert!(
            window.visible_dialog().is_none(),
            "RED: physical remote Back did not dismiss source modal"
        );
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("streams"),
            "remote Back must return to My Channels, not navigate Home"
        );
        let owner = window
            .focused_widget()
            .expect("source-manager Back return owner");
        assert!(
            owner.is_mapped()
                && TvWindow::is_within(&owner, window.imp().streams_view.upcast_ref()),
            "RED: remote Back lost the mapped origin page focus"
        );
        let manager = open();
        assert!(manager.is_mapped());
        native_x11_key("XF86HomePage");
        assert!(
            window.visible_dialog().is_none(),
            "RED: physical remote Home left source modal open"
        );
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("mediathek"),
            "remote Home must navigate to the documented home view"
        );
        let owner = window
            .focused_widget()
            .expect("source-manager Home return owner");
        assert!(
            owner.is_mapped()
                && TvWindow::is_within(&owner, window.imp().mediathek_view.upcast_ref()),
            "RED: Home left an unmapped/orphaned owner after dismissing modal"
        );
        eprintln!("FOCUS001 source_modal genuine_remote Back=return_to_sources Home=dismiss_to_mediathek mapped=1");
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove synthetic source fixture");
    }

    #[test]
    #[ignore = "genuine remote XF86 keys distinguish Guide Back, Home and keyboard Escape"]
    fn gtk_focus001_xtest_guide_manager_actual_remote_back_home_escape() {
        crate::epg::desktop::focus001_seed_disposable_guides(3);
        let (_app, window) = mapped_window("streams");
        assert_native_input_delivery(&window.imp().streams_view.toolbar_focus_targets()[0]);
        let open = || {
            crate::epg::desktop::show_guide_manager(window.upcast_ref::<gtk::Window>());
            focus001_dialog_after(&window, None)
        };
        let manager = open();
        assert!(manager.is_mapped());
        native_x11_key("XF86Back");
        assert!(
            window.visible_dialog().is_none(),
            "RED: real remote Back leaves guide manager open"
        );
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("streams")
        );
        let owner = window.focused_widget().expect("remote guide Back focus");
        assert!(
            owner.is_mapped()
                && TvWindow::is_within(&owner, window.imp().streams_view.upcast_ref()),
            "RED: guide Back did not restore My Channels focus"
        );
        let manager = open();
        assert!(manager.is_mapped());
        native_x11_key("Escape");
        assert!(
            window.visible_dialog().is_none(),
            "keyboard Escape did not close guide manager"
        );
        let manager = open();
        assert!(manager.is_mapped());
        native_x11_key("XF86HomePage");
        assert!(
            window.visible_dialog().is_none(),
            "RED: physical remote Home left guide modal open"
        );
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("mediathek")
        );
        let owner = window.focused_widget().expect("guide Home owner");
        assert!(
            owner.is_mapped()
                && TvWindow::is_within(&owner, window.imp().mediathek_view.upcast_ref()),
            "RED: real guide Home lost the mapped home owner"
        );
        eprintln!("FOCUS001 guide_modal genuine_remote Back=return Escape=close Home=dismiss_home mapped=1");
        window.close();
    }

    // Continuation-008: separate SearchEntry pixel evidence from the existing
    // toolbar *button* pixel probe. The first Right and reverse Left are real
    // XTest events on a mapped window; no `grab_focus` can satisfy reachability.
    // Positive/suppressed controls independently calibrate the framebuffer oracle.
    fn focus001_cont008_painted_search(page: &str, width: i32) {
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
        let search = &controls[0];
        let neighbor = &controls[1];
        assert_native_input_delivery(search);
        assert!(search.is_mapped() && neighbor.is_mapped());
        let bounds = search
            .compute_bounds(window.upcast_ref::<gtk::Widget>())
            .expect("native SearchEntry has mapped bounds");
        // Hash only the four outer border strips, not the GtkText interior:
        // the inner caret paints on focus and would falsify a hidden-ring
        // negative control despite an invisible focus indicator.
        let capture = || {
            let exe = std::env::var_os("FOCUS001_PIXEL_BIN").expect("real X11 oracle required");
            let x = (bounds.x() - 3.0).max(0.0);
            let y = (bounds.y() - 3.0).max(0.0);
            let w = bounds.width() + 6.0;
            let h = bounds.height() + 6.0;
            let strips = [
                (x, y, w, 6.0),
                (x, y + h - 6.0, w, 6.0),
                (x, y + 6.0, 6.0, (h - 12.0).max(1.0)),
                (x + w - 6.0, y + 6.0, 6.0, (h - 12.0).max(1.0)),
            ];
            let mut digests = Vec::new();
            for (px, py, pw, ph) in strips {
                let output = std::process::Command::new(&exe)
                    .args([
                        "--pixels",
                        &px.to_string(),
                        &py.to_string(),
                        &pw.to_string(),
                        &ph.to_string(),
                    ])
                    .output()
                    .expect("X11 focus-ring border capture");
                assert!(
                    output.status.success(),
                    "PIXEL_ENVIRONMENT border bounds/capture"
                );
                digests.push(
                    String::from_utf8(output.stdout)
                        .expect("pixel digest UTF-8")
                        .trim()
                        .to_owned(),
                );
            }
            digests.join(":")
        };
        assert!(search.grab_focus(), "initial SearchEntry focus");
        native_x11_key("Right");
        assert_owner(
            &window,
            neighbor,
            "native Search Right enters next toolbar control",
        );
        let before = capture();
        native_x11_key("Left");
        assert_owner(
            &window,
            search,
            "native neighbour Left restores SearchEntry",
        );
        let after = capture();
        // For independent visual calibration we may seed the *origin* widget;
        // the measured destination is still exclusively reached by native Left.
        let native_focus_search = || {
            assert!(neighbor.grab_focus(), "real mapped left-neighbour origin");
            settle();
            let unfocused = capture();
            native_x11_key("Left");
            assert_owner(&window, search, "native Left focuses SearchEntry");
            (unfocused, capture())
        };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(
            ".focus001-search-positive:focus-within { outline: 4px solid rgb(255,0,255); outline-offset: -4px; box-shadow: none; } \
             .focus001-search-negative, .focus001-search-negative:focus-within, .focus001-search-negative *:focus { outline: none; outline-offset: 0; box-shadow: none; border-color: transparent; }",
        );
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("real X11 display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        search.add_css_class("focus001-search-positive");
        let (positive_before, positive_after) = native_focus_search();
        assert_ne!(
            positive_before, positive_after,
            "RED: explicitly visible SearchEntry focus outline has no measured pixels"
        );
        search.remove_css_class("focus001-search-positive");
        search.add_css_class("focus001-search-negative");
        let (negative_before, negative_after) = native_focus_search();
        assert_eq!(
            negative_before, negative_after,
            "ORACLE_INVALID: suppressed SearchEntry border ring still changes pixels"
        );
        eprintln!("FOCUS001 cont008_search page={page} width={width} real_before={before} real_after={after} calibrated={} suppressed={}", positive_before != positive_after, negative_before == negative_after);
        assert_ne!(
            before, after,
            "RED: native SearchEntry acquired GTK focus without rendered X11 focus change"
        );
        window.close();
    }

    #[test]
    #[ignore = "native mapped SearchEntry rendered pixels and controls, compact Live"]
    fn gtk_focus001_cont008_painted_search_live_compact() {
        focus001_cont008_painted_search("live", 640);
    }
    #[test]
    #[ignore = "native mapped SearchEntry rendered pixels and controls, TV Live"]
    fn gtk_focus001_cont008_painted_search_live_tv() {
        focus001_cont008_painted_search("live", 1280);
    }
    #[test]
    #[ignore = "native mapped SearchEntry rendered pixels and controls, compact Mediathek"]
    fn gtk_focus001_cont008_painted_search_mediathek_compact() {
        focus001_cont008_painted_search("mediathek", 640);
    }
    #[test]
    #[ignore = "native mapped SearchEntry rendered pixels and controls, TV Mediathek"]
    fn gtk_focus001_cont008_painted_search_mediathek_tv() {
        focus001_cont008_painted_search("mediathek", 1280);
    }
    #[test]
    #[ignore = "native mapped SearchEntry rendered pixels and controls, compact My Channels"]
    fn gtk_focus001_cont008_painted_search_streams_compact() {
        focus001_cont008_painted_search("streams", 640);
    }
    #[test]
    #[ignore = "native mapped SearchEntry rendered pixels and controls, TV My Channels"]
    fn gtk_focus001_cont008_painted_search_streams_tv() {
        focus001_cont008_painted_search("streams", 1280);
    }

    // G05/G06 partial evidence: real native manager first-row focus, directional
    // neighbour and independently calibrated actual X11 pixels. Does NOT claim
    // that lower-row controls or nested confirmation focus is covered.
    fn focus001_cont008_manager_pixel(source_mode: bool, width: i32) {
        let (window, dialog, cleanup) = if source_mode {
            let (_app, window, scratch) = focus001_manager_fixture(0);
            window.set_default_size(width, if width == 640 { 480 } else { 800 });
            let dialog = focus001_recovery_open_manager_native(&window);
            (window, dialog, Some(scratch))
        } else {
            crate::epg::desktop::focus001_seed_disposable_guides(0);
            let (_app, window) = mapped_window("streams");
            window.set_default_size(width, if width == 640 { 480 } else { 800 });
            assert_native_input_delivery(&window.imp().streams_view.toolbar_focus_targets()[0]);
            crate::epg::desktop::show_guide_manager(window.upcast_ref::<gtk::Window>());
            let dialog = focus001_dialog_after(&window, None);
            (window, dialog, None)
        };
        let (first_label, second_label) = if source_mode {
            ("Add Playlist", "Programme Guides")
        } else {
            ("Add URL", "Choose Local XMLTV")
        };
        let first = focus001_button_in(&dialog, first_label);
        let second = focus001_button_in(&dialog, second_label);
        assert!(first.is_mapped() && second.is_mapped());
        assert_owner(
            &window,
            first.upcast_ref(),
            "native manager entry first action",
        );
        let bounds = second
            .compute_bounds(window.upcast_ref::<gtk::Widget>())
            .expect("native manager second action bounds");
        let capture = || {
            let exe = std::env::var_os("FOCUS001_PIXEL_BIN").expect("real X11 pixel oracle");
            let params = [
                "--pixels".to_string(),
                (bounds.x() - 3.0).max(0.0).to_string(),
                (bounds.y() - 3.0).max(0.0).to_string(),
                (bounds.width() + 6.0).to_string(),
                (bounds.height() + 6.0).to_string(),
            ];
            let output = std::process::Command::new(exe)
                .args(params)
                .output()
                .expect("X11 manager pixels captured");
            assert!(
                output.status.success(),
                "PIXEL_ENVIRONMENT manager bounds/capture"
            );
            String::from_utf8(output.stdout)
                .expect("UTF8 pixel digest")
                .trim()
                .to_owned()
        };
        let before = capture();
        native_x11_key("Right");
        assert_owner(
            &window,
            second.upcast_ref(),
            "native manager first Right second",
        );
        let after = capture();
        native_x11_key("Left");
        assert_owner(
            &window,
            first.upcast_ref(),
            "native manager second Left reverses",
        );
        let provider = gtk::CssProvider::new();
        provider.load_from_string(".focus001-manager-positive:focus {outline:4px solid rgb(255,0,255);outline-offset:-4px;box-shadow:none;} .focus001-manager-negative,.focus001-manager-negative:focus {outline:none;outline-offset:0;box-shadow:none;}");
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("real X11 display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        second.add_css_class("focus001-manager-positive");
        assert_owner(&window, first.upcast_ref(), "positive manager origin");
        let positive_before = capture();
        native_x11_key("Right");
        assert_owner(
            &window,
            second.upcast_ref(),
            "native positive manager destination",
        );
        let positive_after = capture();
        assert_ne!(
            positive_before, positive_after,
            "ORACLE_INVALID: explicit manager ring has no pixels"
        );
        second.remove_css_class("focus001-manager-positive");
        second.add_css_class("focus001-manager-negative");
        assert!(first.grab_focus(), "local manager origin reseed only");
        settle();
        let negative_before = capture();
        native_x11_key("Right");
        assert_owner(
            &window,
            second.upcast_ref(),
            "native negative manager destination",
        );
        let negative_after = capture();
        assert_eq!(
            negative_before, negative_after,
            "ORACLE_INVALID: hidden manager ring still changes pixels"
        );
        eprintln!("FOCUS001 cont008_manager type={} viewport={width} native_right_left=1 real_change={} calibrated={} suppressed={}", if source_mode {"source"} else {"guide"}, before != after, positive_before != positive_after, negative_before == negative_after);
        assert_ne!(
            before, after,
            "RED: real manager native button focus lacks rendered X11 cue"
        );
        window.close();
        if let Some(path) = cleanup {
            std::fs::remove_dir_all(path).expect("cleanup synthetic manager storage");
        }
    }
    #[test]
    #[ignore = "native source-manager upper toolbar Right/Left and X11 painted focus compact"]
    fn gtk_focus001_cont008_painted_source_manager_compact() {
        focus001_cont008_manager_pixel(true, 640);
    }
    #[test]
    #[ignore = "native source-manager upper toolbar Right/Left and X11 painted focus TV"]
    fn gtk_focus001_cont008_painted_source_manager_tv() {
        focus001_cont008_manager_pixel(true, 1280);
    }
    #[test]
    #[ignore = "native programme-guide manager upper toolbar Right/Left and X11 focus compact"]
    fn gtk_focus001_cont008_painted_guide_manager_compact() {
        focus001_cont008_manager_pixel(false, 640);
    }
    #[test]
    #[ignore = "native programme-guide manager upper toolbar Right/Left and X11 focus TV"]
    fn gtk_focus001_cont008_painted_guide_manager_tv() {
        focus001_cont008_manager_pixel(false, 1280);
    }

    // A card's row is not equivalent to grabbing an individual child action.
    // Traverse from the *native* source-manager entry and require the row's
    // actual mapped focus before testing OK or Right. If it cannot receive
    // focus through D-pad this is a reproducible RED, not a helper-only pass.
    fn focus001_cont008_source_card_portal(width: i32, enter: &str) {
        let (_app, window, scratch) = focus001_manager_fixture(1);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let manager = focus001_recovery_open_manager_native(&window);
        let add = focus001_button_in(&manager, "Add Playlist");
        assert_owner(
            &window,
            add.upcast_ref(),
            "source modal actual first action",
        );
        let mut list_row = None;
        for _ in 0..35 {
            settle();
            if let Some(widget) = TvWindow::find_descendant(manager.upcast_ref(), &|widget| {
                widget.downcast_ref::<gtk::ListBoxRow>().is_some() && widget.is_mapped()
            }) {
                list_row = Some(widget);
                break;
            }
        }
        let row = list_row.expect("synthetic source row must be mapped");
        let mut key_trace = Vec::<String>::new();
        let mut row_reached = false;
        for _ in 0..9 {
            let focused = window.focused_widget().expect("modal D-pad owner");
            key_trace.push(focused.type_().name().to_string());
            if TvWindow::is_within(&focused, &row) {
                row_reached = true;
                break;
            }
            native_x11_key("Down");
        }
        eprintln!("FOCUS001 CONT008 source_portal viewport={width} key={enter} native_down_owners={key_trace:?} reached_row={row_reached}");
        assert!(
            row_reached,
            "RED: source card row cannot be entered from native manager launch"
        );
        let current = window.focused_widget().expect("native row owner");
        assert!(
            row == current,
            "RED: source card only focuses an individual child, not row OK/Right portal"
        );
        native_x11_key(enter);
        let target = window
            .focused_widget()
            .expect("native row portal destination");
        assert!(target.is_mapped() && target.is_sensitive());
        assert!(
            TvWindow::is_within(&target, &row),
            "RED: row portal escaped its source card"
        );
        let switch = TvWindow::find_descendant(&row, &|widget| widget.is::<gtk::Switch>())
            .expect("source row has enabled first safe switch action");
        if enter == "Return" {
            assert_owner(
                &window,
                &switch,
                "source card OK must focus first safe action",
            );
        } else {
            // The lower row must be reachable from the row by Right, then
            // reverse Left must preserve the *same* logical source card.
            let lower = TvWindow::find_descendant(&row, &|widget| {
                widget.downcast_ref::<gtk::Button>().is_some_and(|button| {
                    matches!(
                        button.label().as_deref(),
                        Some("Replace" | "Delete" | "↑" | "↓")
                    ) && widget.is_mapped()
                        && widget.is_sensitive()
                }) && TvWindow::is_within(&target, widget)
            });
            assert!(
                lower.is_some(),
                "RED: source card Right portal did not reach lower actions"
            );
            native_x11_key("Left");
            assert!(
                window
                    .focused_widget()
                    .as_ref()
                    .is_some_and(|owner| { TvWindow::is_within(owner, &row) && owner.is_mapped() }),
                "RED: source lower-action Left lost original card row"
            );
        }
        native_x11_key("Escape");
        assert!(
            window.visible_dialog().is_none(),
            "modal Escape must close after card navigation"
        );
        window.close();
        std::fs::remove_dir_all(scratch).expect("cleanup only disposable source fixture");
    }
    #[test]
    #[ignore = "native source row OK entry versus first safe action, compact"]
    fn gtk_focus001_cont008_source_card_ok_compact() {
        focus001_cont008_source_card_portal(640, "Return");
    }
    #[test]
    #[ignore = "native source row OK entry versus first safe action, TV"]
    fn gtk_focus001_cont008_source_card_ok_tv() {
        focus001_cont008_source_card_portal(1280, "Return");
    }
    #[test]
    #[ignore = "native source row Right lower action and Left reverse, compact"]
    fn gtk_focus001_cont008_source_card_right_compact() {
        focus001_cont008_source_card_portal(640, "Right");
    }
    #[test]
    #[ignore = "native source row Right lower action and Left reverse, TV"]
    fn gtk_focus001_cont008_source_card_right_tv() {
        focus001_cont008_source_card_portal(1280, "Right");
    }

    // E01/E02: full native Live first/middle/last row journey. Every mapped
    // Live card is a real production Card backed by a unique fabricated
    // ChannelObject; no provider traffic, mpv or playback action is invoked.
    fn focus001_cont008_live_row_graph(width: i32, portal: Option<&str>) {
        let (_app, window) = mapped_window("live");
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let live = &window.imp().live_view;
        live.show_toolbar_for_test();
        let search = live.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        let rows = live.focus001_install_disposable_live_rows(5);
        settle();
        assert!(rows.iter().all(|(row, _)| row.is_mapped()));
        assert!(search.grab_focus());
        let mut origin_trace = Vec::<String>::new();
        let mut entered = false;
        for _ in 0..12 {
            let owner = window.focused_widget().expect("real Live native focus");
            origin_trace.push(owner.type_().name().to_owned());
            if TvWindow::is_within(&owner, rows[0].0.upcast_ref()) {
                entered = true;
                break;
            }
            native_x11_key("Down");
        }
        eprintln!("FOCUS001 CONT008 live width={width} portal={portal:?} entry_owners={origin_trace:?} first_reached={entered}");
        assert!(
            entered,
            "RED: Live Search cannot natively enter first populated ListBox row"
        );
        for (row, _) in rows.iter().skip(1) {
            native_x11_key("Down");
            assert_owner(
                &window,
                row.upcast_ref(),
                "native Live Down must reach next real ListBoxRow",
            );
        }
        for idx in (0..4).rev() {
            native_x11_key("Up");
            assert_owner(
                &window,
                rows[idx].0.upcast_ref(),
                "native Live Up must reverse actual ListBoxRow",
            );
        }
        if let Some(enter) = portal {
            // Enter the *middle and last* rows through native Down, rather
            // than grabbing their Play button as a fake reachability proof.
            for index in [2usize, 4] {
                for _ in 0..index {
                    native_x11_key("Down");
                }
                assert_owner(
                    &window,
                    rows[index].0.upcast_ref(),
                    "actual Live portal row",
                );
                native_x11_key(enter);
                assert_owner(
                    &window,
                    rows[index].1.upcast_ref(),
                    "RED: Live card portal should focus Play without activating playback",
                );
                // Escape must restore the same logical Live row for the next
                // mapped native probe (never perform a second Play OK here).
                native_x11_key("Escape");
                assert_owner(
                    &window,
                    rows[index].0.upcast_ref(),
                    "Live card Escape row owner",
                );
                for _ in 0..index {
                    native_x11_key("Up");
                }
                assert_owner(
                    &window,
                    rows[0].0.upcast_ref(),
                    "Live row reset via native Up",
                );
            }
        }
        eprintln!("FOCUS001 CONT008 live_native rows=5 viewport={width} middle_last=1 reverse=4 portal={portal:?}");
        window.close();
    }
    #[test]
    #[ignore = "five unique real Live rows via Search Down then first-middle-last reverse compact"]
    fn gtk_focus001_cont008_live_five_rows_compact() {
        focus001_cont008_live_row_graph(640, None);
    }
    #[test]
    #[ignore = "five unique real Live rows via Search Down then first-middle-last reverse TV"]
    fn gtk_focus001_cont008_live_five_rows_tv() {
        focus001_cont008_live_row_graph(1280, None);
    }
    #[test]
    #[ignore = "native Live first-middle-last row OK portal direct Play focus compact"]
    fn gtk_focus001_cont008_live_middle_last_ok_compact() {
        focus001_cont008_live_row_graph(640, Some("Return"));
    }
    #[test]
    #[ignore = "native Live first-middle-last row OK portal direct Play focus TV"]
    fn gtk_focus001_cont008_live_middle_last_ok_tv() {
        focus001_cont008_live_row_graph(1280, Some("Return"));
    }
    #[test]
    #[ignore = "native Live first-middle-last row Right portal direct Play focus compact"]
    fn gtk_focus001_cont008_live_middle_last_right_compact() {
        focus001_cont008_live_row_graph(640, Some("Right"));
    }
    #[test]
    #[ignore = "native Live first-middle-last row Right portal direct Play focus TV"]
    fn gtk_focus001_cont008_live_middle_last_right_tv() {
        focus001_cont008_live_row_graph(1280, Some("Right"));
    }

    // Characterization only: native X11 Page_Down/End/Home bindings must be
    // distinguished from a remote D-pad walk. No helper scroll_to or focus
    // seeding after initial ListView entry. An unimplemented binding is RED,
    // not automatically credited to the D38 10k/15k remote journey.
    fn focus001_cont008_native_far_key_probe(total: u32) {
        let (_app, window) = mapped_window("streams");
        window.set_default_size(640, 480);
        settle();
        let streams = &window.imp().streams_view;
        let search = streams.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        let original = streams.focus001_mapped_virtual_page(total, 0);
        settle();
        assert!(streams.show_and_focus_test_results());
        settle();
        assert_eq!(streams.focus001_selected_index(), 0);
        native_x11_key("Next"); // Real GDK PageDown key, not programmatic scroll.
        let after_page = streams.focus001_selected_index();
        let page_owner = window.focused_widget().expect("native PageDown focus");
        assert!(
            page_owner.is_mapped() && streams.contains_results_focus(&page_owner),
            "RED: native PageDown orphaned focus at {after_page}"
        );
        eprintln!("FOCUS001 native_far total={total} key=Next first_index={after_page}");
        assert!(
            after_page > 0 && after_page < total,
            "RED: native PageDown did not select a subsequent real row"
        );
        native_x11_key("End"); // Characterize real GTK end-of-list binding.
        settle();
        let at_end = streams.focus001_selected_index();
        eprintln!("FOCUS001 native_far total={total} key=End selected={at_end}");
        assert_eq!(
            at_end,
            total - 1,
            "RED: native End did not reach last selected row"
        );
        let owner = window.focused_widget().expect("native End focus owner");
        assert!(
            owner.is_mapped() && owner.is_sensitive() && streams.contains_results_focus(&owner),
            "RED: native End left an unmapped row"
        );
        native_x11_key("Up");
        assert_eq!(
            streams.focus001_selected_index(),
            total - 2,
            "RED: native Up from last did not select penultimate row"
        );
        native_x11_key("Down");
        assert_eq!(
            streams.focus001_selected_index(),
            total - 1,
            "RED: native Down did not reverse to last row"
        );
        native_x11_key("Home");
        settle();
        assert_eq!(
            streams.focus001_selected_index(),
            0,
            "RED: native Home did not return to first row"
        );
        assert_eq!(
            streams.remote_action_test_selected_id(),
            Some(original),
            "RED: first-row source-qualified ID lost after native page/end/home"
        );
        let (slots, pages, count) = streams.focus001_mapped_virtual_stats();
        assert_eq!(count, total);
        assert!(
            slots <= total as usize && pages <= (total / 128 + 2) as usize,
            "RED: virtual slots/pages exceeded bounded model after PageDown/End/Home"
        );
        eprintln!("FOCUS001 native_far total={total} first=0 page={after_page} last={} return=0 slots={slots} pages={pages} KEYBOARD_PAGE_END_HOME_NOT_REMOTE_DPAD",total-1);
        window.close();
    }

    #[test]
    #[ignore = "native GTK ListView 1484 PageDown/End/Home characterization, not remote D-pad"]
    fn gtk_focus001_cont008_native_far_1484_page_end_home() {
        focus001_cont008_native_far_key_probe(1_484);
    }
    #[test]
    #[ignore = "native GTK ListView 10000 PageDown/End/Home characterization, not remote D-pad"]
    fn gtk_focus001_cont008_native_far_10000_page_end_home() {
        focus001_cont008_native_far_key_probe(10_000);
    }
    #[test]
    #[ignore = "native GTK ListView 15000 PageDown/End/Home characterization, not remote D-pad"]
    fn gtk_focus001_cont008_native_far_15000_page_end_home() {
        focus001_cont008_native_far_key_probe(15_000);
    }

    // C09: reach the My Channels keyboard from the Search field exclusively
    // through native D-pad routing, not direct focus injection on the launcher.
    fn focus001_cont008_streams_osk_remote(width: i32) {
        let (_app, window) = mapped_window("streams");
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        settle();
        let controls = window.imp().streams_view.toolbar_focus_targets();
        let search = controls[0].clone();
        let keyboard = controls[11].clone();
        assert!(keyboard.is_mapped() && keyboard.is_sensitive());
        assert_native_input_delivery(&search);
        for key in ["XF86Back", "Escape", "XF86HomePage"] {
            assert!(search.grab_focus(), "initial mapped SearchEntry fixture");
            let mut visited = Vec::new();
            let mut reached = false;
            for _ in 0..16 {
                let owner = window.focused_widget().expect("native toolbar owner");
                visited.push(owner.type_().name().to_string());
                if owner == keyboard || TvWindow::is_within(&owner, &keyboard) {
                    reached = true;
                    break;
                }
                native_x11_key("Right");
            }
            eprintln!("FOCUS001 streams_osk_remote viewport={width} key={key} search_to_keyboard={reached} trace={visited:?}");
            assert!(
                reached,
                "RED: My Channels keyboard launcher unreachable by native D-pad from Search"
            );
            native_x11_key("Return");
            let dialog = window
                .visible_dialog()
                .expect("RED: native keyboard OK did not open OSK");
            assert!(dialog.is_mapped());
            let owner = window.focused_widget().expect("OSK focus on open");
            assert!(owner.is_mapped() && TvWindow::is_within(&owner, dialog.upcast_ref()));
            native_x11_key("Down");
            let owner = window.focused_widget().expect("OSK focus on Down");
            assert!(
                owner.is_mapped() && TvWindow::is_within(&owner, dialog.upcast_ref()),
                "RED: OSK Down escaped its modal"
            );
            native_x11_key(key);
            settle();
            assert!(
                window.visible_dialog().is_none(),
                "RED: remote {key} did not dismiss OSK"
            );
            assert_eq!(
                window.imp().stack.visible_child_name().as_deref(),
                Some("streams"),
                "RED: OSK special key navigated behind My Channels modal"
            );
            let owner = window.focused_widget().expect("OSK return owner");
            assert!(
                owner.is_mapped()
                    && owner.is_sensitive()
                    && TvWindow::is_within(
                        &owner,
                        window.imp().stack.visible_child().unwrap().upcast_ref()
                    ),
                "RED: OSK special key failed valid My Channels focus restoration"
            );
        }
        eprintln!("FOCUS001 streams_osk_remote viewport={width} native_route=Search_Right_to_Keyboard keys=XF86Back_Escape_XF86HomePage");
        window.close();
    }
    #[test]
    #[ignore = "native My Channels compact search-to-keyboard and remote Back/Escape/Home focus"]
    fn gtk_focus001_cont008_streams_osk_remote_compact() {
        focus001_cont008_streams_osk_remote(640);
    }
    #[test]
    #[ignore = "native My Channels TV search-to-keyboard and remote Back/Escape/Home focus"]
    fn gtk_focus001_cont008_streams_osk_remote_tv() {
        focus001_cont008_streams_osk_remote(1280);
    }

    // A/AG08: a fully connected native Search -> Manage Sources -> Programme
    // Guides journey, with separate physical Back/Escape/Home semantics. The
    // old guide modal tests opened the dialog directly or seeded action focus.
    fn focus001_cont008_connected_guide_remote_return(width: i32, key: &str) {
        crate::epg::desktop::focus001_seed_disposable_guides(3);
        let (_app, window, scratch) = focus001_manager_fixture(0);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        settle();
        let source = focus001_recovery_open_manager_native(&window);
        let button = focus001_button_in(&source, "Programme Guides");
        let mut visited = Vec::<String>::new();
        let mut reached = false;
        for _ in 0..8 {
            let owner = window
                .focused_widget()
                .expect("source manager action owner");
            visited.push(owner.type_().name().to_string());
            if TvWindow::is_within(&owner, button.upcast_ref()) {
                reached = true;
                break;
            }
            native_x11_key("Right");
        }
        eprintln!("FOCUS001 connected_guide_return viewport={width} remote={key} reached={reached} source_actions={visited:?}");
        assert!(
            reached,
            "RED: real source-manager opener to Programme Guides unreachable"
        );
        native_x11_key("Return");
        let guide = focus001_dialog_after(&window, Some(&source));
        let owner = window.focused_widget().expect("native opened guide focus");
        assert!(owner.is_mapped() && TvWindow::is_within(&owner, guide.upcast_ref()));
        native_x11_key(key);
        settle();
        match key {
            "Escape" | "XF86Back" => {
                assert_eq!(
                    window.visible_dialog(),
                    Some(source.clone()),
                    "RED: nested guide remote Back/Escape did not return source manager"
                );
                let owner = window.focused_widget().expect("manager return owner");
                assert!(
                    owner.is_mapped() && TvWindow::is_within(&owner, source.upcast_ref()),
                    "RED: nested guide dismissal orphaned source-manager focus"
                );
                native_x11_key("Escape");
                settle();
                assert!(
                    window.visible_dialog().is_none(),
                    "RED: final source-manager Escape failed"
                );
                let owner = window.focused_widget().expect("source page returned owner");
                assert!(
                    owner.is_mapped()
                        && TvWindow::is_within(&owner, window.imp().streams_view.upcast_ref()),
                    "RED: manager return did not retain mapped My Channels origin"
                );
            }
            "XF86HomePage" => {
                assert!(
                    window.visible_dialog().is_none(),
                    "RED: remote Home left nested source/guide modal visible"
                );
                assert_eq!(
                    window.imp().stack.visible_child_name().as_deref(),
                    Some("mediathek"),
                    "RED: remote Home did not return to home view"
                );
                let owner = window.focused_widget().expect("native Home focus");
                assert!(
                    owner.is_mapped()
                        && TvWindow::is_within(&owner, window.imp().mediathek_view.upcast_ref()),
                    "RED: remote Home left orphaned focus"
                );
            }
            _ => unreachable!(),
        }
        eprintln!(
            "FOCUS001 connected_guide_return viewport={width} remote={key} nested_source_return=1"
        );
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove disposable source fixture");
    }
    #[test]
    #[ignore = "native connected guide nested Escape compact return owner"]
    fn gtk_focus001_cont008_connected_guide_escape_compact() {
        focus001_cont008_connected_guide_remote_return(640, "Escape");
    }
    #[test]
    #[ignore = "native connected guide nested Escape TV return owner"]
    fn gtk_focus001_cont008_connected_guide_escape_tv() {
        focus001_cont008_connected_guide_remote_return(1280, "Escape");
    }
    #[test]
    #[ignore = "native connected guide nested remote Back compact return owner"]
    fn gtk_focus001_cont008_connected_guide_back_compact() {
        focus001_cont008_connected_guide_remote_return(640, "XF86Back");
    }
    #[test]
    #[ignore = "native connected guide nested remote Back TV return owner"]
    fn gtk_focus001_cont008_connected_guide_back_tv() {
        focus001_cont008_connected_guide_remote_return(1280, "XF86Back");
    }
    #[test]
    #[ignore = "native connected guide nested remote Home compact home view"]
    fn gtk_focus001_cont008_connected_guide_home_compact() {
        focus001_cont008_connected_guide_remote_return(640, "XF86HomePage");
    }
    #[test]
    #[ignore = "native connected guide nested remote Home TV home view"]
    fn gtk_focus001_cont008_connected_guide_home_tv() {
        focus001_cont008_connected_guide_remote_return(1280, "XF86HomePage");
    }
    // D08/D09: exercise the application menu from a real focused SearchEntry.
    // The X11 Menu keysym must reach the window capture handler; neither the
    // MenuButton nor its popover can be focused/opened directly to satisfy this
    // connected native route. Three distinct dismissal semantics run in
    // separate isolated processes to retain evidence even when one is RED.
    fn focus001_cont008_native_app_menu(page: &str, width: i32, key: &str) {
        let (_app, window) = mapped_window(page);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        if page == "live" {
            window.imp().live_view.show_toolbar_for_test();
        }
        settle();
        let targets = match page {
            "live" => window.imp().live_view.toolbar_focus_targets(),
            "mediathek" => window.imp().mediathek_view.toolbar_focus_targets(),
            "streams" => window.imp().streams_view.toolbar_focus_targets(),
            _ => unreachable!(),
        };
        let search = &targets[0];
        assert_native_input_delivery(search);
        assert_owner(&window, search, "native menu journey Search origin");
        let menu = window.imp().menu_button.get();
        let model = menu.menu_model().expect("real menu model required");
        assert_eq!(model.n_items(), 1, "application menu section count changed");
        let section = model
            .item_link(0, "section")
            .expect("real menu section with actions");
        assert_eq!(
            section.n_items(),
            6,
            "application menu action inventory changed"
        );
        native_x11_key("Menu");
        let popover = menu.popover().expect("application menu popover");
        assert!(
            popover.is_mapped(),
            "RED: physical Menu key did not open application menu"
        );
        let owner = window
            .focused_widget()
            .expect("RED: menu has no native focus owner");
        assert!(
            owner.is_mapped() && TvWindow::is_within(&owner, popover.upcast_ref()),
            "RED: physical Menu opened popup without putting focus into it: {}",
            owner.type_().name()
        );
        let before = owner.clone();
        native_x11_key("Down");
        let moved = window
            .focused_widget()
            .expect("RED: popover Down orphaned focus");
        assert!(
            moved.is_mapped() && TvWindow::is_within(&moved, popover.upcast_ref()),
            "RED: popover native Down escaped focus confinement"
        );
        eprintln!("FOCUS001 menu_native page={page} viewport={width} key={key} before={} after={} same={}",
            before.type_().name(), moved.type_().name(), before == moved);
        native_x11_key(key);
        settle();
        assert!(
            !popover.is_mapped(),
            "RED: {key} did not dismiss active app menu"
        );
        let expected_page = if key == "XF86HomePage" {
            "mediathek"
        } else {
            page
        };
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some(expected_page),
            "RED: {key} changed the page unexpectedly / failed Home semantics"
        );
        let restored = window
            .focused_widget()
            .expect("RED: popover dismissal orphaned focus");
        assert!(
            restored.is_mapped() && restored.is_sensitive(),
            "RED: menu dismissal returned to unmapped or disabled focus"
        );
        assert!(
            !TvWindow::is_within(&restored, popover.upcast_ref()),
            "RED: menu dismissal retained focus inside unmapped popover"
        );
        eprintln!("FOCUS001 menu_native page={page} viewport={width} key={key} closed=1 destination={expected_page} owner={}", restored.type_().name());
        window.close();
    }
    #[test]
    #[ignore = "native XTest app menu live compact physical back and focus owner"]
    fn gtk_focus001_cont008_app_menu_live_compact_back() {
        focus001_cont008_native_app_menu("live", 640, "XF86Back");
    }
    #[test]
    #[ignore = "native XTest app menu live compact physical escape and focus owner"]
    fn gtk_focus001_cont008_app_menu_live_compact_escape() {
        focus001_cont008_native_app_menu("live", 640, "Escape");
    }
    #[test]
    #[ignore = "native XTest app menu live compact physical home and focus owner"]
    fn gtk_focus001_cont008_app_menu_live_compact_home() {
        focus001_cont008_native_app_menu("live", 640, "XF86HomePage");
    }
    #[test]
    #[ignore = "native XTest app menu live tv physical back and focus owner"]
    fn gtk_focus001_cont008_app_menu_live_tv_back() {
        focus001_cont008_native_app_menu("live", 1280, "XF86Back");
    }
    #[test]
    #[ignore = "native XTest app menu live tv physical escape and focus owner"]
    fn gtk_focus001_cont008_app_menu_live_tv_escape() {
        focus001_cont008_native_app_menu("live", 1280, "Escape");
    }
    #[test]
    #[ignore = "native XTest app menu live tv physical home and focus owner"]
    fn gtk_focus001_cont008_app_menu_live_tv_home() {
        focus001_cont008_native_app_menu("live", 1280, "XF86HomePage");
    }
    #[test]
    #[ignore = "native XTest app menu mediathek compact physical back and focus owner"]
    fn gtk_focus001_cont008_app_menu_mediathek_compact_back() {
        focus001_cont008_native_app_menu("mediathek", 640, "XF86Back");
    }
    #[test]
    #[ignore = "native XTest app menu mediathek compact physical escape and focus owner"]
    fn gtk_focus001_cont008_app_menu_mediathek_compact_escape() {
        focus001_cont008_native_app_menu("mediathek", 640, "Escape");
    }
    #[test]
    #[ignore = "native XTest app menu mediathek compact physical home and focus owner"]
    fn gtk_focus001_cont008_app_menu_mediathek_compact_home() {
        focus001_cont008_native_app_menu("mediathek", 640, "XF86HomePage");
    }
    #[test]
    #[ignore = "native XTest app menu mediathek tv physical back and focus owner"]
    fn gtk_focus001_cont008_app_menu_mediathek_tv_back() {
        focus001_cont008_native_app_menu("mediathek", 1280, "XF86Back");
    }
    #[test]
    #[ignore = "native XTest app menu mediathek tv physical escape and focus owner"]
    fn gtk_focus001_cont008_app_menu_mediathek_tv_escape() {
        focus001_cont008_native_app_menu("mediathek", 1280, "Escape");
    }
    #[test]
    #[ignore = "native XTest app menu mediathek tv physical home and focus owner"]
    fn gtk_focus001_cont008_app_menu_mediathek_tv_home() {
        focus001_cont008_native_app_menu("mediathek", 1280, "XF86HomePage");
    }
    #[test]
    #[ignore = "native XTest app menu streams compact physical back and focus owner"]
    fn gtk_focus001_cont008_app_menu_streams_compact_back() {
        focus001_cont008_native_app_menu("streams", 640, "XF86Back");
    }
    #[test]
    #[ignore = "native XTest app menu streams compact physical escape and focus owner"]
    fn gtk_focus001_cont008_app_menu_streams_compact_escape() {
        focus001_cont008_native_app_menu("streams", 640, "Escape");
    }
    #[test]
    #[ignore = "native XTest app menu streams compact physical home and focus owner"]
    fn gtk_focus001_cont008_app_menu_streams_compact_home() {
        focus001_cont008_native_app_menu("streams", 640, "XF86HomePage");
    }
    #[test]
    #[ignore = "native XTest app menu streams tv physical back and focus owner"]
    fn gtk_focus001_cont008_app_menu_streams_tv_back() {
        focus001_cont008_native_app_menu("streams", 1280, "XF86Back");
    }
    #[test]
    #[ignore = "native XTest app menu streams tv physical escape and focus owner"]
    fn gtk_focus001_cont008_app_menu_streams_tv_escape() {
        focus001_cont008_native_app_menu("streams", 1280, "Escape");
    }
    #[test]
    #[ignore = "native XTest app menu streams tv physical home and focus owner"]
    fn gtk_focus001_cont008_app_menu_streams_tv_home() {
        focus001_cont008_native_app_menu("streams", 1280, "XF86HomePage");
    }
    // A GApplication-registered, single-window fixture is essential for the
    // actual app.* menu actions. The older direct TvWindow::new fixture can
    // inadvertently create a second window when app.about uses app.window().
    fn focus001_registered_menu_fixture() -> (TvApplication, TvWindow, std::path::PathBuf) {
        let resource = gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
            .expect("valid bundled GTK resources");
        gio::resources_register(&resource);
        gtk::init().expect("Xvfb GTK display required");
        let app = TvApplication::new();
        app.register(None::<&gio::Cancellable>)
            .expect("register disposable GTK application for native app menu actions");
        let window = app.window();
        window.set_default_size(1280, 800);
        window.imp().stack.set_visible_child_name("streams");
        window.present();
        settle();
        let scratch = std::env::temp_dir().join(format!("focus001-menu-{}", uuid::Uuid::new_v4()));
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&scratch)
            .expect("private disposable test-only SQLite directory");
        let path = scratch.join("manager.sqlite");
        crate::streams::view::TvStreamsView::focus001_seed_disposable_manager_sources(
            path.clone(),
            0,
        );
        let view = &window.imp().streams_view;
        let ready = view.focus001_attach_disposable_service(path);
        for _ in 0..50 {
            if ready.get() {
                break;
            }
            settle();
        }
        assert!(ready.get(), "disposable menu source manager not ready");
        assert_native_input_delivery(&view.toolbar_focus_targets()[0]);
        (app, window, scratch)
    }

    // D08 connected native activation of two safe, disposable menu actions.
    // The real Menu key, not direct action.activate() or MenuButton.popup(),
    // opens the global menu. All focus/selection keys are XTest-injected.
    fn focus001_cont008_menu_activate_source_action(width: i32, action: &str) {
        let (_app, window, scratch) = focus001_registered_menu_fixture();
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        settle();
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        assert_owner(&window, &search, "native app menu action Search origin");
        native_x11_key("Menu");
        let popover = window
            .imp()
            .menu_button
            .popover()
            .expect("real global menu");
        assert!(
            popover.is_mapped(),
            "RED: physical Menu key did not open menu"
        );
        let menu_owner = window.focused_widget().expect("native menu owner");
        assert!(
            menu_owner.is_mapped() && TvWindow::is_within(&menu_owner, popover.upcast_ref()),
            "RED: menu did not claim focus before native action selection"
        );
        // Items are Preferences, Add Playlist, Manage Sources, ... .
        let down_count = match action {
            "Add Playlist" => 1,
            "Manage Sources" => 2,
            _ => unreachable!(),
        };
        for step in 0..down_count {
            native_x11_key("Down");
            let owner = window.focused_widget().expect("menu item focus");
            let mut widget = Some(owner.clone());
            while let Some(w) = widget {
                eprintln!("FOCUS001 menu_path action={action} down_step={} type={} widget_name={} label={:?} text={:?}",
                    step+1, w.type_().name(), w.widget_name(), w.downcast_ref::<gtk::Button>().and_then(|b| b.label()),
                    if w.find_property("text").is_some() { Some(w.property::<String>("text")) } else { None });
                if w == *popover.upcast_ref::<gtk::Widget>() {
                    break;
                }
                widget = w.parent();
            }
            assert!(
                owner.is_mapped() && TvWindow::is_within(&owner, popover.upcast_ref()),
                "RED: native selection escaped menu before action activation"
            );
        }
        let selected = window.focused_widget().expect("native menu selected item");
        assert_eq!(
            selected
                .find_property("text")
                .map(|_| selected.property::<String>("text")),
            Some(action.to_string()),
            "RED: native arrows did not reach requested real menu action"
        );
        native_x11_key("Return");
        eprintln!("FOCUS001 menu_after_return action={action} popover_mapped={} visible_dialog={} page={:?} owner_type={:?}",
            popover.is_mapped(), window.visible_dialog().is_some(), window.imp().stack.visible_child_name(),
            window.focused_widget().map(|w| w.type_().name()));
        let dialog = focus001_dialog_after(&window, None);
        assert!(
            !popover.is_mapped(),
            "RED: activating menu item left menu open"
        );
        let focused = window
            .focused_widget()
            .expect("native menu action dialog owner");
        assert!(
            focused.is_mapped() && TvWindow::is_within(&focused, dialog.upcast_ref()),
            "RED: native menu {action} did not focus newly activated dialog"
        );
        eprintln!("FOCUS001 menu_action viewport={width} action={action} native_down={down_count} dialog={} focus={}",
            dialog.type_().name(), focused.type_().name());
        native_x11_key("XF86Back");
        settle();
        assert!(
            window.visible_dialog().is_none(),
            "RED: physical Back failed to dismiss menu-activated {action} dialog"
        );
        let returned = window.focused_widget().expect("menu action return owner");
        eprintln!("FOCUS001 menu_action_return viewport={width} action={action} owner={} mapped={} visible={} sensitive={} focus={} page={:?}",
            returned.type_().name(), returned.is_mapped(), returned.is_visible(), returned.is_sensitive(), returned.has_focus(), window.imp().stack.visible_child_name());
        assert!(
            returned.is_mapped()
                && TvWindow::is_within(&returned, window.imp().streams_view.upcast_ref()),
            "RED: menu action Back failed to return to My Channels"
        );
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove private disposable sources");
    }
    #[test]
    #[ignore = "native global menu Add Playlist activation and physical Back at 640"]
    fn gtk_focus001_cont008_menu_action_add_playlist_compact() {
        focus001_cont008_menu_activate_source_action(640, "Add Playlist");
    }
    #[test]
    #[ignore = "native global menu Add Playlist activation and physical Back at 1280"]
    fn gtk_focus001_cont008_menu_action_add_playlist_tv() {
        focus001_cont008_menu_activate_source_action(1280, "Add Playlist");
    }
    #[test]
    #[ignore = "native global menu Manage Sources activation and physical Back at 640"]
    fn gtk_focus001_cont008_menu_action_manage_sources_compact() {
        focus001_cont008_menu_activate_source_action(640, "Manage Sources");
    }
    #[test]
    #[ignore = "native global menu Manage Sources activation and physical Back at 1280"]
    fn gtk_focus001_cont008_menu_action_manage_sources_tv() {
        focus001_cont008_menu_activate_source_action(1280, "Manage Sources");
    }
    // FOCUS-001 continuation-009: the other four production application-menu
    // items must be activated through the connected native Menu/Down/OK path,
    // not merely counted in a GMenuModel. Each runs in its own Xvfb process.
    fn focus001_cont009_menu_extra_action(width: i32, label: &str) {
        let (_app, window, scratch) = focus001_registered_menu_fixture();
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        settle();
        let search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        let n = match label {
            "Preferences" => 0,
            "Toggle Fullscreen" => 3,
            "Keyboard Shortcuts" => 4,
            "About Broadcaster" => 5,
            _ => unreachable!(),
        };
        let menu = window.imp().menu_button.get();
        let root = menu.menu_model().expect("live GMenu model");
        let section = root.item_link(0, "section").expect("live menu section");
        let actual = section
            .item_attribute_value(n, "label", None)
            .and_then(|value| value.str().map(ToOwned::to_owned))
            .expect("label for real menu item");
        assert_eq!(actual, label, "menu item enumeration changed");
        native_x11_key("Menu");
        let popover = menu.popover().expect("native menu popover");
        assert!(popover.is_mapped(), "RED: native Menu did not open popover");
        for _ in 0..n {
            native_x11_key("Down");
            let current = window.focused_widget().expect("menu item focus owner");
            assert!(
                current.is_mapped() && TvWindow::is_within(&current, popover.upcast_ref()),
                "RED: native menu arrow left the active menu"
            );
        }
        let selected = window.focused_widget().expect("native menu selected item");
        assert_eq!(
            selected
                .find_property("text")
                .map(|_| selected.property::<String>("text")),
            Some(label.to_string()),
            "RED: native menu Down path reached wrong or disabled action"
        );
        native_x11_key("Return");
        settle();
        assert!(
            !popover.is_mapped(),
            "RED: native OK did not activate {label}"
        );
        if label == "Toggle Fullscreen" {
            let mut state = window.is_fullscreen();
            for _ in 0..20 {
                if state {
                    break;
                }
                settle();
                state = window.is_fullscreen();
            }
            eprintln!("FOCUS001 menu_extra viewport={width} action={label} fullscreen={state}");
            assert!(
                state,
                "RED: enabled native Fullscreen menu action had no effect"
            );
            native_x11_key("Menu");
            assert!(popover.is_mapped(), "RED: cannot reopen menu in fullscreen");
            for _ in 0..3 {
                native_x11_key("Down");
            }
            let selected = window
                .focused_widget()
                .expect("fullscreen second menu target");
            assert_eq!(
                selected
                    .find_property("text")
                    .map(|_| selected.property::<String>("text")),
                Some(label.to_string()),
                "RED: fullscreen second menu target changed"
            );
            native_x11_key("Return");
            settle();
            assert!(
                !window.is_fullscreen(),
                "RED: second native Fullscreen action failed to restore windowed mode"
            );
        } else {
            let dialog = focus001_dialog_after(&window, None);
            let owner = window.focused_widget().expect("menu dialog focus owner");
            assert!(
                owner.is_mapped() && TvWindow::is_within(&owner, dialog.upcast_ref()),
                "RED: activated {label} without confined mapped dialog focus"
            );
            native_x11_key("XF86Back");
            settle();
            assert!(
                window.visible_dialog().is_none(),
                "RED: {label} native Back did not dismiss dialog"
            );
        }
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("streams"),
            "RED: {label} native activation/return switched away from invoking view"
        );
        let returned = window.focused_widget().expect("menu extra return owner");
        eprintln!(
            "FOCUS001 menu_extra viewport={width} action={label} returned={} mapped={} page={:?}",
            returned.type_().name(),
            returned.is_mapped(),
            window.imp().stack.visible_child_name()
        );
        assert!(
            returned.is_mapped()
                && returned.is_sensitive()
                && TvWindow::is_within(&returned, window.imp().streams_view.upcast_ref()),
            "RED: {label} left native focus outside invoking My Channels view"
        );
        window.close();
        std::fs::remove_dir_all(scratch).expect("delete disposable menu fixture");
    }

    #[test]
    #[ignore = "XTest native Preferences menu activation and return at compact"]
    fn gtk_focus001_cont009_menu_preferences_compact() {
        focus001_cont009_menu_extra_action(640, "Preferences");
    }
    #[test]
    #[ignore = "XTest native Preferences menu activation and return at TV"]
    fn gtk_focus001_cont009_menu_preferences_tv() {
        focus001_cont009_menu_extra_action(1280, "Preferences");
    }
    #[test]
    #[ignore = "XTest native Fullscreen menu activation and reverse at compact"]
    fn gtk_focus001_cont009_menu_fullscreen_compact() {
        focus001_cont009_menu_extra_action(640, "Toggle Fullscreen");
    }
    #[test]
    #[ignore = "XTest native Fullscreen menu activation and reverse at TV"]
    fn gtk_focus001_cont009_menu_fullscreen_tv() {
        focus001_cont009_menu_extra_action(1280, "Toggle Fullscreen");
    }
    #[test]
    #[ignore = "XTest native Shortcuts menu activation and return at compact"]
    fn gtk_focus001_cont009_menu_shortcuts_compact() {
        focus001_cont009_menu_extra_action(640, "Keyboard Shortcuts");
    }
    #[test]
    #[ignore = "XTest native Shortcuts menu activation and return at TV"]
    fn gtk_focus001_cont009_menu_shortcuts_tv() {
        focus001_cont009_menu_extra_action(1280, "Keyboard Shortcuts");
    }
    #[test]
    #[ignore = "XTest native About menu activation and return at compact"]
    fn gtk_focus001_cont009_menu_about_compact() {
        focus001_cont009_menu_extra_action(640, "About Broadcaster");
    }
    #[test]
    #[ignore = "XTest native About menu activation and return at TV"]
    fn gtk_focus001_cont009_menu_about_tv() {
        focus001_cont009_menu_extra_action(1280, "About Broadcaster");
    }

    // Continuation-012 D08: run every enabled global-menu action from the
    // *other* two pages. Earlier My Channels checks do not prove its portal is
    // equivalent to Live/Mediathek. Real Menu/Down/Return/Back keys, one
    // separately initialized mapped GTK process per action and page.
    fn focus001_cont012_menu_action_from_other_page(page: &str, label: &str) {
        let (_app, window, scratch) = focus001_registered_menu_fixture();
        let width = if page == "live" || page == "streams" {
            640
        } else {
            1280
        };
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        window.imp().stack.set_visible_child_name(page);
        if page == "live" {
            // Synthetic offline Live starts on its loading stack; the actual
            // displayed channel toolbar must be enabled *before* native keys.
            window.imp().live_view.show_toolbar_for_test();
        }
        settle();
        let page_widget: &gtk::Widget = match page {
            "live" => window.imp().live_view.upcast_ref(),
            "mediathek" => window.imp().mediathek_view.upcast_ref(),
            "streams" => window.imp().streams_view.upcast_ref(),
            _ => unreachable!("not an authorized D08 page"),
        };
        let search = match page {
            "live" => window.imp().live_view.toolbar_focus_targets()[0].clone(),
            "mediathek" => window.imp().mediathek_view.toolbar_focus_targets()[0].clone(),
            "streams" => window.imp().streams_view.toolbar_focus_targets()[0].clone(),
            _ => unreachable!(),
        };
        assert_native_input_delivery(&search);
        assert_owner(&window, &search, "D08 genuine invoking-page Search");
        let target = match label {
            "Preferences" => 0,
            "Add Playlist" => 1,
            "Manage Sources" => 2,
            "Toggle Fullscreen" => 3,
            "Keyboard Shortcuts" => 4,
            "About Broadcaster" => 5,
            _ => unreachable!("unrecognized main menu item"),
        };
        let menu = window.imp().menu_button.get();
        let root = menu.menu_model().expect("real app menu");
        let section = root.item_link(0, "section").expect("real app menu section");
        assert_eq!(
            section
                .item_attribute_value(target, "label", None)
                .and_then(|value| value.str().map(ToOwned::to_owned)),
            Some(label.to_string()),
            "D08 menu production labels/order changed"
        );
        native_x11_key("Menu");
        let popup = menu.popover().expect("real mapped global menu");
        assert!(popup.is_mapped(), "RED: Menu key not opening from {page}");
        for _ in 0..target {
            native_x11_key("Down");
            let focused = window
                .focused_widget()
                .expect("menu focus lost during native Down");
            assert!(focused.is_mapped() && TvWindow::is_within(&focused, popup.upcast_ref()));
        }
        let selected = window.focused_widget().expect("menu selected action");
        assert_eq!(
            selected
                .find_property("text")
                .map(|_| selected.property::<String>("text")),
            Some(label.to_string()),
            "RED: real Down route did not focus enabled selected app action"
        );
        native_x11_key("Return");
        settle();
        assert!(!popup.is_mapped(), "RED: menu action did not dismiss popup");
        if label == "Toggle Fullscreen" {
            let mut entered = window.is_fullscreen();
            for _ in 0..20 {
                if entered {
                    break;
                }
                settle();
                entered = window.is_fullscreen();
            }
            assert!(entered, "RED: Fullscreen action failed from {page}");
            native_x11_key("Menu");
            assert!(
                popup.is_mapped(),
                "RED: menu cannot reopen from fullscreen {page}"
            );
            for _ in 0..3 {
                native_x11_key("Down");
            }
            let selected = window
                .focused_widget()
                .expect("fullscreen return menu focus");
            assert_eq!(
                selected
                    .find_property("text")
                    .map(|_| selected.property::<String>("text")),
                Some(label.to_owned())
            );
            native_x11_key("Return");
            settle();
            assert!(
                !window.is_fullscreen(),
                "RED: fullscreen did not exit from {page}"
            );
        } else {
            let dialog = focus001_dialog_after(&window, None);
            let focused = window.focused_widget().expect("new menu dialog focus");
            assert!(
                focused.is_mapped() && TvWindow::is_within(&focused, dialog.upcast_ref()),
                "RED: enabled menu {label} opened without a mapped focused dialog from {page}"
            );
            native_x11_key("XF86Back");
            settle();
            assert!(
                window.visible_dialog().is_none(),
                "RED: Back failed to dismiss {label} from {page}"
            );
        }
        let returned = window.focused_widget().expect("post-menu focus owner");
        eprintln!("FOCUS001 cont012 D08 page={page} width={width} label={label} mapped={} page_after={:?} returned_type={} returned_in_page={}",
            returned.is_mapped(), window.imp().stack.visible_child_name(),
            returned.type_().name(), TvWindow::is_within(&returned, page_widget));
        // Add/Manage are intentionally window.show-streams actions in
        // production. Their destination page is not a focus regression.
        let expected_page = if label == "Add Playlist" || label == "Manage Sources" {
            "streams"
        } else {
            page
        };
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some(expected_page),
            "RED: menu {label} navigated away from documented destination"
        );
        let expected_owner_page: &gtk::Widget = if expected_page == "streams" {
            window.imp().streams_view.upcast_ref()
        } else {
            page_widget
        };
        assert!(
            returned.is_mapped()
                && returned.is_sensitive()
                && TvWindow::is_within(&returned, expected_owner_page),
            "RED: menu {label} dismissed, but focus escaped its documented destination from {page}"
        );
        window.close();
        std::fs::remove_dir_all(scratch).expect("cleanup synthetic private menu fixture");
    }

    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from live on Preferences"]
    fn gtk_focus001_cont012_menu_live_preferences() {
        focus001_cont012_menu_action_from_other_page("live", "Preferences");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from live on Add Playlist"]
    fn gtk_focus001_cont012_menu_live_add_playlist() {
        focus001_cont012_menu_action_from_other_page("live", "Add Playlist");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from live on Manage Sources"]
    fn gtk_focus001_cont012_menu_live_manage_sources() {
        focus001_cont012_menu_action_from_other_page("live", "Manage Sources");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from live on Toggle Fullscreen"]
    fn gtk_focus001_cont012_menu_live_fullscreen() {
        focus001_cont012_menu_action_from_other_page("live", "Toggle Fullscreen");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from live on Keyboard Shortcuts"]
    fn gtk_focus001_cont012_menu_live_shortcuts() {
        focus001_cont012_menu_action_from_other_page("live", "Keyboard Shortcuts");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from live on About Broadcaster"]
    fn gtk_focus001_cont012_menu_live_about() {
        focus001_cont012_menu_action_from_other_page("live", "About Broadcaster");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from mediathek on Preferences"]
    fn gtk_focus001_cont012_menu_mediathek_preferences() {
        focus001_cont012_menu_action_from_other_page("mediathek", "Preferences");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from mediathek on Add Playlist"]
    fn gtk_focus001_cont012_menu_mediathek_add_playlist() {
        focus001_cont012_menu_action_from_other_page("mediathek", "Add Playlist");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from mediathek on Manage Sources"]
    fn gtk_focus001_cont012_menu_mediathek_manage_sources() {
        focus001_cont012_menu_action_from_other_page("mediathek", "Manage Sources");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from mediathek on Toggle Fullscreen"]
    fn gtk_focus001_cont012_menu_mediathek_fullscreen() {
        focus001_cont012_menu_action_from_other_page("mediathek", "Toggle Fullscreen");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from mediathek on Keyboard Shortcuts"]
    fn gtk_focus001_cont012_menu_mediathek_shortcuts() {
        focus001_cont012_menu_action_from_other_page("mediathek", "Keyboard Shortcuts");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from mediathek on About Broadcaster"]
    fn gtk_focus001_cont012_menu_mediathek_about() {
        focus001_cont012_menu_action_from_other_page("mediathek", "About Broadcaster");
    }

    // Third-page D08 coverage: the same real Menu/Down/OK/Back journey,
    // from the authentic mapped My Channels Search origin.
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from My Channels on Preferences"]
    fn gtk_focus001_cont012_menu_streams_preferences() {
        focus001_cont012_menu_action_from_other_page("streams", "Preferences");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from My Channels on Add Playlist"]
    fn gtk_focus001_cont012_menu_streams_add_playlist() {
        focus001_cont012_menu_action_from_other_page("streams", "Add Playlist");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from My Channels on Manage Sources"]
    fn gtk_focus001_cont012_menu_streams_manage_sources() {
        focus001_cont012_menu_action_from_other_page("streams", "Manage Sources");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from My Channels on Toggle Fullscreen"]
    fn gtk_focus001_cont012_menu_streams_fullscreen() {
        focus001_cont012_menu_action_from_other_page("streams", "Toggle Fullscreen");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from My Channels on Keyboard Shortcuts"]
    fn gtk_focus001_cont012_menu_streams_shortcuts() {
        focus001_cont012_menu_action_from_other_page("streams", "Keyboard Shortcuts");
    }
    #[test]
    #[ignore = "native XTest global Menu/Down/OK/Back from My Channels on About Broadcaster"]
    fn gtk_focus001_cont012_menu_streams_about() {
        focus001_cont012_menu_action_from_other_page("streams", "About Broadcaster");
    }

    // D09: native opening and dismissal of a GTK DropDown popup. This tests
    // a different GTK capture surface from the global GtkPopoverMenu: the
    // user starts at Search and reaches the source/channel dropdown by arrows.
    fn focus001_cont008_native_dropdown_dismiss(page: &str, width: i32, key: &str) {
        let (_app, window) = mapped_window(page);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        settle();
        let targets = match page {
            "mediathek" => window.imp().mediathek_view.toolbar_focus_targets(),
            "streams" => window.imp().streams_view.toolbar_focus_targets(),
            _ => unreachable!(),
        };
        let search = targets[0].clone();
        assert_native_input_delivery(&search);
        let dropdown = targets
            .iter()
            .find_map(|widget| {
                widget
                    .clone()
                    .downcast::<gtk::DropDown>()
                    .ok()
                    .filter(|d| d.is_mapped() && d.is_sensitive())
            })
            .expect("real enabled source/broadcaster dropdown");
        let before_selected = dropdown.selected();
        // Replay each candidate route from the real mapped Search origin, with
        // every transition delivered through XTest. A fixed six-key sample
        // previously missed Source after Search -> Down -> Group -> Left,
        // despite the independently executed toolbar graph proving that edge.
        // Never grab_focus on an intermediate control or the target.
        let mut pending = std::collections::VecDeque::from([Vec::<&str>::new()]);
        let mut visited = Vec::<gtk::Widget>::new();
        let mut reached_path = None;
        let mut replayed = 0_usize;
        while let Some(candidate_path) = pending.pop_front() {
            assert!(search.grab_focus(), "mapped Search must accept focus");
            settle();
            for direction in &candidate_path {
                native_x11_key(direction);
                replayed += 1;
            }
            let owner = window
                .focused_widget()
                .expect("RED: native dropdown Search route orphaned focus");
            assert!(
                owner.is_mapped() && owner.is_sensitive(),
                "RED: native dropdown route reached an unmapped/disabled owner"
            );
            if TvWindow::is_within(&owner, dropdown.upcast_ref()) {
                reached_path = Some(candidate_path);
                break;
            }
            if candidate_path.len() >= 4 || visited.iter().any(|w| w == &owner) {
                continue;
            }
            visited.push(owner);
            for arrow in ["Up", "Down", "Left", "Right"] {
                let mut next = candidate_path.clone();
                next.push(arrow);
                pending.push_back(next);
            }
        }
        eprintln!(
            "FOCUS001 dropdown_native page={page} width={width} reached={} native_replayed={replayed} search_path={reached_path:?}",
            reached_path.is_some()
        );
        assert!(
            reached_path.is_some(),
            "RED: enabled dropdown has no connected native Search route within a four-arrow toolbar graph"
        );
        native_x11_key("space");
        let popup = TvWindow::find_mapped_popover(window.upcast_ref())
            .expect("RED: native dropdown Space did not open a mapped popover");
        let owner = window
            .focused_widget()
            .expect("RED: dropdown popup missing focus");
        assert!(
            owner.is_mapped() && TvWindow::is_within(&owner, popup.upcast_ref()),
            "RED: native dropdown popover opened without confined focus"
        );
        native_x11_key("Down");
        let inside = window
            .focused_widget()
            .expect("RED: popup Down orphaned focus");
        assert!(
            inside.is_mapped() && TvWindow::is_within(&inside, popup.upcast_ref()),
            "RED: popup Down escaped mapped options"
        );
        native_x11_key(key);
        settle();
        assert!(
            !popup.is_mapped(),
            "RED: {key} failed to dismiss native dropdown popup"
        );
        let expected_page = if key == "XF86HomePage" {
            "mediathek"
        } else {
            page
        };
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some(expected_page),
            "RED: dropdown dismissal changed the wrong view"
        );
        let returned = window
            .focused_widget()
            .expect("RED: dropdown dismissal orphaned focus");
        assert!(
            returned.is_mapped() && returned.is_sensitive(),
            "RED: dropdown dismissal returned to invalid focus"
        );
        assert_eq!(
            dropdown.selected(),
            before_selected,
            "RED: cancelling/dropdown Home changed selection unexpectedly"
        );
        eprintln!("FOCUS001 dropdown_native page={page} width={width} key={key} selected={before_selected} destination={expected_page}");
        window.close();
    }
    #[test]
    #[ignore = "native mediathek dropdown popup compact physical back dismissal"]
    fn gtk_focus001_cont008_dropdown_mediathek_compact_back() {
        focus001_cont008_native_dropdown_dismiss("mediathek", 640, "XF86Back");
    }
    #[test]
    #[ignore = "native mediathek dropdown popup compact physical escape dismissal"]
    fn gtk_focus001_cont008_dropdown_mediathek_compact_escape() {
        focus001_cont008_native_dropdown_dismiss("mediathek", 640, "Escape");
    }
    #[test]
    #[ignore = "native mediathek dropdown popup compact physical home dismissal"]
    fn gtk_focus001_cont008_dropdown_mediathek_compact_home() {
        focus001_cont008_native_dropdown_dismiss("mediathek", 640, "XF86HomePage");
    }
    #[test]
    #[ignore = "native mediathek dropdown popup tv physical back dismissal"]
    fn gtk_focus001_cont008_dropdown_mediathek_tv_back() {
        focus001_cont008_native_dropdown_dismiss("mediathek", 1280, "XF86Back");
    }
    #[test]
    #[ignore = "native mediathek dropdown popup tv physical escape dismissal"]
    fn gtk_focus001_cont008_dropdown_mediathek_tv_escape() {
        focus001_cont008_native_dropdown_dismiss("mediathek", 1280, "Escape");
    }
    #[test]
    #[ignore = "native mediathek dropdown popup tv physical home dismissal"]
    fn gtk_focus001_cont008_dropdown_mediathek_tv_home() {
        focus001_cont008_native_dropdown_dismiss("mediathek", 1280, "XF86HomePage");
    }
    #[test]
    #[ignore = "native streams dropdown popup compact physical back dismissal"]
    fn gtk_focus001_cont008_dropdown_streams_compact_back() {
        focus001_cont008_native_dropdown_dismiss("streams", 640, "XF86Back");
    }
    #[test]
    #[ignore = "native streams dropdown popup compact physical escape dismissal"]
    fn gtk_focus001_cont008_dropdown_streams_compact_escape() {
        focus001_cont008_native_dropdown_dismiss("streams", 640, "Escape");
    }
    #[test]
    #[ignore = "native streams dropdown popup compact physical home dismissal"]
    fn gtk_focus001_cont008_dropdown_streams_compact_home() {
        focus001_cont008_native_dropdown_dismiss("streams", 640, "XF86HomePage");
    }
    #[test]
    #[ignore = "native streams dropdown popup tv physical back dismissal"]
    fn gtk_focus001_cont008_dropdown_streams_tv_back() {
        focus001_cont008_native_dropdown_dismiss("streams", 1280, "XF86Back");
    }
    #[test]
    #[ignore = "native streams dropdown popup tv physical escape dismissal"]
    fn gtk_focus001_cont008_dropdown_streams_tv_escape() {
        focus001_cont008_native_dropdown_dismiss("streams", 1280, "Escape");
    }
    #[test]
    #[ignore = "native streams dropdown popup tv physical home dismissal"]
    fn gtk_focus001_cont008_dropdown_streams_tv_home() {
        focus001_cont008_native_dropdown_dismiss("streams", 1280, "XF86HomePage");
    }

    // Continuation-010: separate real D-pad reachability from the persistence
    // of a locally seeded, native-OK action. The second measurement does NOT
    // convert an unreachable card into a connected remote path.
    fn focus001_cont010_reorder_source(direction: i32, width: i32) {
        assert!(matches!(direction, -1 | 1));
        let (_app, window, scratch) = focus001_manager_fixture(3);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let store = crate::streams::storage::Storage::at(scratch.join("manager.sqlite"));
        let original = store
            .list_sources()
            .expect("private source order before test");
        let before: Vec<_> = original.iter().map(|source| source.id.clone()).collect();
        assert_eq!(before.len(), 3);
        let manager = focus001_recovery_open_manager_native(&window);
        let list =
            TvWindow::find_descendant(manager.upcast_ref(), &|widget| widget.is::<gtk::ListBox>())
                .expect("real manager ListBox")
                .downcast::<gtk::ListBox>()
                .expect("GtkListBox");
        let middle = (0..50)
            .find_map(|_| {
                settle();
                list.row_at_index(1)
            })
            .expect("three actual source cards in manager");
        let arrow_label = if direction == -1 { "↑" } else { "↓" };
        let target = TvWindow::find_descendant(middle.upcast_ref(), &|widget| {
            widget
                .downcast_ref::<gtk::Button>()
                .is_some_and(|button| button.label().as_deref() == Some(arrow_label))
        })
        .expect("specific second-source reorder action");
        assert!(target.is_mapped() && target.is_sensitive());
        let target_bounds = target
            .compute_bounds(&window)
            .expect("mapped reorder-button rectangle");
        assert!(target_bounds.width() > 0.0 && target_bounds.height() > 0.0);

        // Survey each currently mapped control's *real XTest* arrow edges,
        // then replay an observed route starting at the dialog's actual
        // focus after opening. Survey seeds are never counted as reachability.
        let mut controls = Vec::<gtk::Widget>::new();
        let mut stack = vec![manager.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = stack.pop() {
            if widget.is_mapped()
                && widget.is_visible()
                && widget.is_sensitive()
                && widget.is_focusable()
                && (widget.is::<gtk::Button>() || widget.is::<gtk::Switch>())
            {
                controls.push(widget.clone());
            }
            let mut child = widget.first_child();
            while let Some(item) = child {
                child = item.next_sibling();
                stack.push(item);
            }
        }
        let identify = |owner: &gtk::Widget| {
            controls
                .iter()
                .enumerate()
                .find_map(|(i, widget)| TvWindow::is_within(owner, widget).then_some(i))
        };
        let start_owner = window.focused_widget().expect("manager opened with focus");
        let start = identify(&start_owner).expect("manager opened on a mapped control");
        let goal = controls
            .iter()
            .position(|control| control == &target)
            .expect("target is in mapped manager controls");
        let mut edges = vec![Vec::<(usize, &'static str)>::new(); controls.len()];
        for i in 0..controls.len() {
            for key in ["Up", "Down", "Left", "Right"] {
                assert!(controls[i].grab_focus(), "survey origin must be mapped");
                native_x11_key(key);
                if let Some(j) = window.focused_widget().and_then(|w| identify(&w)) {
                    if i != j {
                        edges[i].push((j, key));
                    }
                }
            }
        }
        let mut parent = vec![None::<(usize, &'static str)>; controls.len()];
        let mut seen = vec![false; controls.len()];
        seen[start] = true;
        let mut queue = std::collections::VecDeque::from([start]);
        while let Some(i) = queue.pop_front() {
            for &(j, key) in &edges[i] {
                if !seen[j] {
                    seen[j] = true;
                    parent[j] = Some((i, key));
                    queue.push_back(j);
                }
            }
        }
        let connected = seen[goal];
        let mut route = Vec::<(usize, usize, &'static str)>::new();
        if connected {
            let mut cursor = goal;
            while cursor != start {
                let (previous, key) = parent[cursor].expect("BFS parent");
                route.push((previous, cursor, key));
                cursor = previous;
            }
            route.reverse();
            assert!(
                controls[start].grab_focus(),
                "restore authentic dialog entry"
            );
            for &(from, to, key) in &route {
                assert_owner(&window, &controls[from], "native reorder route origin");
                native_x11_key(key);
                assert_owner(&window, &controls[to], "native reorder route destination");
            }
        } else {
            // Diagnostic action only. This seed cannot satisfy the original
            // owner's native D-pad reachability requirement.
            assert!(
                target.grab_focus(),
                "local action probe needs mapped control"
            );
        }
        native_x11_key("Return");
        let expected = if direction == -1 {
            vec![before[1].clone(), before[0].clone(), before[2].clone()]
        } else {
            vec![before[0].clone(), before[2].clone(), before[1].clone()]
        };
        let mut after = before.clone();
        for _ in 0..60 {
            settle();
            after = store
                .list_sources()
                .expect("private source order after native OK")
                .iter()
                .map(|source| source.id.clone())
                .collect();
            if after == expected {
                break;
            }
        }
        let saved = after == expected;
        let focus = window.focused_widget();
        let focus_inside = focus.as_ref().is_some_and(|owner| {
            owner.is_mapped()
                && owner.is_sensitive()
                && TvWindow::is_within(owner, manager.upcast_ref())
        });
        eprintln!(
            "FOCUS001 cont010 source_reorder direction={direction} viewport={width}x{} source_id={} controls={} connected={} route={route:?} SQLite_saved={} manager_focus={} seeded_diagnostic={}",
            if width == 640 { 480 } else { 800 },
            before[1], controls.len(), connected, saved, focus_inside, !connected
        );
        window.close();
        std::fs::remove_dir_all(scratch).expect("delete synthetic reorder fixture");
        assert!(
            saved,
            "RED: mapped native OK failed to persist source reorder"
        );
        assert!(focus_inside, "RED: source reorder orphaned manager focus");
        assert!(
            connected,
            "RED: source reorder cannot be reached by a real connected D-pad route"
        );
    }

    #[test]
    #[ignore = "native source Move Up route and seeded-only SQLite diagnostic, 640x480"]
    fn gtk_focus001_cont010_source_move_up_compact() {
        focus001_cont010_reorder_source(-1, 640);
    }

    #[test]
    #[ignore = "native source Move Down route and seeded-only SQLite diagnostic, 1280x800"]
    fn gtk_focus001_cont010_source_move_down_tv() {
        focus001_cont010_reorder_source(1, 1280);
    }

    // D38 AG09: enter *both* managers through the real page D-pad portal,
    // not through a direct show_guide_manager fixture, and test the nested
    // Add dialog before asserting the complete 3-guide modal arrow graph.
    fn focus001_cont010_guide_manager_connected_nested(width: i32) {
        crate::epg::desktop::focus001_seed_disposable_guides(3);
        let (_app, window, scratch) = focus001_manager_fixture(1);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let source_manager = focus001_recovery_open_manager_native(&window);
        let add_source = focus001_button_in(&source_manager, "Add Playlist");
        let guides = focus001_button_in(&source_manager, "Programme Guides");
        assert_owner(
            &window,
            add_source.upcast_ref(),
            "source manager native entry Add Playlist",
        );
        native_x11_key("Right");
        assert_owner(
            &window,
            guides.upcast_ref(),
            "native manager header Right reaches Programme Guides",
        );
        native_x11_key("Return");
        let guide_manager = focus001_dialog_after(&window, Some(&source_manager));
        let add_url = focus001_button_in(&guide_manager, "Add URL");
        let local = focus001_button_in(&guide_manager, "Choose Local XMLTV");
        let close = focus001_button_in(&guide_manager, "Close");
        assert!(add_url.is_mapped() && local.is_mapped() && close.is_mapped());
        assert_owner(&window, add_url.upcast_ref(), "guide manager entry Add URL");
        native_x11_key("Right");
        assert_owner(
            &window,
            local.upcast_ref(),
            "guide manager native right chooser",
        );
        native_x11_key("Left");
        assert_owner(
            &window,
            add_url.upcast_ref(),
            "guide manager native left reverse",
        );
        native_x11_key("Return");
        let nested = focus001_dialog_after(&window, Some(&guide_manager));
        let owner = window
            .focused_widget()
            .expect("native nested guide form focus");
        assert!(owner.is_mapped() && TvWindow::is_within(&owner, nested.upcast_ref()));
        native_x11_key("Escape");
        assert_eq!(window.visible_dialog(), Some(guide_manager.clone()));
        let owner = window
            .focused_widget()
            .expect("guide form Escape return focus");
        assert!(owner.is_mapped() && TvWindow::is_within(&owner, guide_manager.upcast_ref()));
        eprintln!("FOCUS001 cont010 guide_portal viewport={width}x{} source_to_guide=connected add_nested=true return=true guide_fixture=3", if width == 640 { 480 } else { 800 });
        // The existing graph oracle rejects unreachable lower-row actions.
        // A failure is a genuine RED, never permission to seed them as PASS.
        focus001_graph_root_controls(&window, &guide_manager, false, 3, true);
        native_x11_key("Escape");
        assert_eq!(window.visible_dialog(), Some(source_manager.clone()));
        native_x11_key("Escape");
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove private source fixture");
    }

    #[test]
    #[ignore = "D38 AG09: native cross-manager guide grid, nested modal and reverse at 640x480"]
    fn gtk_focus001_cont010_guide_manager_nested_compact() {
        focus001_cont010_guide_manager_connected_nested(640);
    }

    #[test]
    #[ignore = "native cross-manager guide grid, nested modal and reverse at 1280x800"]
    fn gtk_focus001_cont010_guide_manager_nested_tv() {
        focus001_cont010_guide_manager_connected_nested(1280);
    }

    // D38 D06: use the real active Live tab as the first anchor, traverse
    // Mediathek and My Channels, exercise the last tab, then reverse. A
    // compact AdwViewSwitcher can deliberately route a boundary arrow into
    // a separate header control; those arrows are not required to be end-stop
    // loops. Never assume GTK child enumeration is visible tab order.
    #[test]
    #[ignore = "native compact first/last page switcher activations with connected Down/Up page portals"]
    fn gtk_focus001_cont010_global_switcher_boundaries_compact() {
        let (_app, window) = mapped_window("live");
        window.set_default_size(640, 480);
        window.imp().live_view.show_toolbar_for_test();
        settle();
        let search = window.imp().live_view.toolbar_focus_targets()[0].clone();
        assert_native_input_delivery(&search);
        native_x11_key("Up");
        let first = window.focused_widget().expect("first native tab portal");
        assert_eq!(first.type_().name(), "AdwViewSwitcherButton");
        assert_owner(&window, &first, "Search Up enters real Live category");
        native_x11_key("Return");
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("live")
        );
        native_x11_key("Down");
        assert_owner(&window, &search, "first tab Down to Live search");
        native_x11_key("Up");
        assert_owner(&window, &first, "first tab reverse Up from Live search");

        native_x11_key("Right");
        let middle = window.focused_widget().expect("middle native category");
        assert_eq!(middle.type_().name(), "AdwViewSwitcherButton");
        assert_ne!(middle, first);
        native_x11_key("Return");
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("mediathek")
        );
        native_x11_key("Down");
        let mediathek_search = window.imp().mediathek_view.toolbar_focus_targets()[0].clone();
        assert_owner(
            &window,
            &mediathek_search,
            "middle tab Down to Mediathek search",
        );
        native_x11_key("Up");
        assert_owner(
            &window,
            &middle,
            "middle tab reverse Up from Mediathek search",
        );

        native_x11_key("Right");
        let last = window.focused_widget().expect("last native category");
        assert_eq!(last.type_().name(), "AdwViewSwitcherButton");
        assert_ne!(last, middle);
        assert_ne!(last, first);
        native_x11_key("Return");
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("streams")
        );
        native_x11_key("Down");
        let streams_search = window.imp().streams_view.toolbar_focus_targets()[0].clone();
        assert_owner(
            &window,
            &streams_search,
            "last tab Down to My Channels search",
        );
        native_x11_key("Up");
        assert_owner(
            &window,
            &last,
            "last tab reverse Up from My Channels search",
        );

        native_x11_key("Left");
        assert_owner(&window, &middle, "last tab physical Left returns to middle");
        native_x11_key("Left");
        assert_owner(&window, &first, "middle tab physical Left returns to first");
        native_x11_key("Return");
        assert_eq!(
            window.imp().stack.visible_child_name().as_deref(),
            Some("live")
        );
        native_x11_key("Down");
        assert_owner(&window, &search, "first tab restored Live portal");
        eprintln!("FOCUS001 cont010 D06 viewport=640x480 three_categories=connected first=live middle=mediathek last=streams native_LR_OK_DownUp=true origin_restored=true; boundary arrows beyond first/last unasserted");
        window.close();
    }

    // Continuation-011: native guide editor and removal workflow from real manager entry.
    // The graph survey may seed an origin only to discover its local arrow
    // edges. Every selected action is subsequently reached by replaying the
    // entire path from the real Add URL modal-entry focus using XTest keys.
    fn focus001_cont011_guide_editor_or_remove(width: i32, action: &str) {
        crate::epg::desktop::focus001_seed_disposable_guides(1);
        let (_app, window, scratch) = focus001_manager_fixture(1);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let source_manager = focus001_recovery_open_manager_native(&window);
        assert_owner(
            &window,
            focus001_button_in(&source_manager, "Add Playlist").upcast_ref(),
            "guide workflow source-manager portal",
        );
        native_x11_key("Right");
        assert_owner(
            &window,
            focus001_button_in(&source_manager, "Programme Guides").upcast_ref(),
            "guide workflow Programme Guides real Right",
        );
        native_x11_key("Return");
        let dialog = focus001_dialog_after(&window, Some(&source_manager));
        let labels = [
            "Add URL",
            "Choose Local XMLTV",
            "Close",
            "Use as Shared Default",
            "Refresh",
            "Edit",
            "Remove",
        ];
        let controls: Vec<gtk::Widget> = labels
            .iter()
            .map(|name| focus001_button_in(&dialog, name).upcast::<gtk::Widget>())
            .collect();
        let identify = |w: &gtk::Widget| -> Option<usize> {
            controls
                .iter()
                .position(|button| TvWindow::is_within(w, button))
        };
        let start_owner = window
            .focused_widget()
            .expect("guide authentic entry focus");
        let start = identify(&start_owner).expect("guide native focus is an enabled control");
        let target_name = if action == "RemoveConfirmed" {
            "Remove"
        } else {
            action
        };
        let target = labels
            .iter()
            .position(|name| *name == target_name)
            .expect("named action");
        let mut edges = vec![Vec::<(usize, &'static str)>::new(); labels.len()];
        for (i, control) in controls.iter().enumerate() {
            assert!(control.is_mapped() && control.is_sensitive());
            for key in ["Up", "Down", "Left", "Right"] {
                assert!(control.grab_focus(), "local edge probe origin mapped");
                native_x11_key(key);
                if let Some(j) = window.focused_widget().and_then(|w| identify(&w)) {
                    if i != j {
                        edges[i].push((j, key));
                    }
                }
            }
        }
        let mut previous = vec![None::<(usize, &'static str)>; labels.len()];
        let mut discovered = vec![false; labels.len()];
        let mut queue = std::collections::VecDeque::from([start]);
        discovered[start] = true;
        while let Some(i) = queue.pop_front() {
            for &(j, key) in &edges[i] {
                if !discovered[j] {
                    discovered[j] = true;
                    previous[j] = Some((i, key));
                    queue.push_back(j);
                }
            }
        }
        let mut route = vec![];
        if discovered[target] {
            let mut i = target;
            while i != start {
                let (from, key) = previous[i].expect("connected parent");
                route.push((from, i, key));
                i = from;
            }
            route.reverse();
            assert!(controls[start].grab_focus(), "restore authentic entry");
            for &(from, to, key) in &route {
                assert_owner(&window, &controls[from], "guide target route origin");
                native_x11_key(key);
                assert_owner(
                    &window,
                    &controls[to],
                    "guide target route reached destination",
                );
            }
            native_x11_key("Return");
            let nested = focus001_dialog_after(&window, Some(&dialog));
            let owner = window.focused_widget().expect("nested guide owner");
            assert!(owner.is_mapped() && TvWindow::is_within(&owner, nested.upcast_ref()));
            if action == "Remove" || action == "RemoveConfirmed" {
                let keep = focus001_button_in(&nested, "Keep Guide");
                let permanently = focus001_button_in(&nested, "Remove Permanently");
                assert!(keep.is_mapped() && permanently.is_mapped());
                // Genuine native Enter from the default Keep Guide control;
                // do not seed Keep or claim confirmation of deletion.
                assert_owner(
                    &window,
                    keep.upcast_ref(),
                    "guide remove confirmation default Keep",
                );
                if action == "RemoveConfirmed" {
                    native_x11_key("Down");
                    assert_owner(
                        &window,
                        permanently.upcast_ref(),
                        "guide destructive confirmation native Down",
                    );
                }
                native_x11_key("Return");
                if action == "RemoveConfirmed" {
                    let registry = crate::epg::sources::GuideSourceRegistry::new(
                        crate::streams::storage::Storage::new(),
                    );
                    let mut remaining = registry
                        .list()
                        .expect("read disposable guide registry")
                        .len();
                    for _ in 0..60 {
                        if remaining == 0 {
                            break;
                        }
                        settle();
                        remaining = registry.list().expect("read async guide deletion").len();
                    }
                    eprintln!(
                        "FOCUS001 cont011 confirmed guide removal registry_remaining={remaining}"
                    );
                    assert_eq!(
                        remaining, 0,
                        "RED: native confirmed delete did not remove last guide"
                    );
                }
            } else {
                let entry = TvWindow::find_descendant(nested.upcast_ref(), &|w| {
                    w.downcast_ref::<gtk::Entry>().is_some()
                })
                .expect("guide Edit contains a named entry");
                assert!(entry.is_mapped() && entry.is_sensitive());
                native_x11_key("Escape");
            }
            assert_eq!(
                window.visible_dialog(),
                Some(dialog.clone()),
                "nested guide cancellation must return to manager"
            );
            let returned = window.focused_widget();
            let focus_valid = returned.as_ref().is_some_and(|widget| {
                widget.is_mapped()
                    && widget.is_sensitive()
                    && TvWindow::is_within(widget, dialog.upcast_ref())
            });
            eprintln!("FOCUS001 cont011 native_guide_action={} viewport={}x{} connected=true route={route:?} return_focus_valid={} focus_type={:?}", action, width, if width == 640 {480} else {800}, focus_valid, returned.as_ref().map(|widget| widget.type_().name()));
            assert!(focus_valid, "RED: native guide action removed/closed nested control but did not restore focus to mapped guide manager");
        } else {
            eprintln!("FOCUS001 cont011 native_guide_action={} viewport={}x{} connected=false, no activation or seeded-success fallback", action, width, if width == 640 {480} else {800});
        }
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove disposable guide fixture");
        assert!(
            discovered[target],
            "RED: enabled guide action not reachable with native D-pad"
        );
    }

    #[test]
    #[ignore = "AG04 partial: connected native guide Edit dialog entry/Escape at compact viewport"]
    fn gtk_focus001_cont011_guide_edit_compact() {
        focus001_cont011_guide_editor_or_remove(640, "Edit");
    }

    #[test]
    #[ignore = "AG05 partial: connected native guide Remove/Keep confirmation at TV viewport"]
    fn gtk_focus001_cont011_guide_remove_keep_tv() {
        focus001_cont011_guide_editor_or_remove(1280, "Remove");
    }

    #[test]
    #[ignore = "AG05: connected native confirmed deletion of last disposable guide and focus return, 640x480"]
    fn gtk_focus001_cont011_guide_remove_confirm_compact() {
        focus001_cont011_guide_editor_or_remove(640, "RemoveConfirmed");
    }

    // AG02: From an authentic native guide-manager entry, replay the actual
    // D-pad route to Enabled and Apply. The survey may seed intermediate
    // mapped origins to DISCOVER edges, but only the replay from the current
    // true focus owns the action and may count as native reachability.
    // Walk the REAL native focus graph by replaying full arrow routes from the
    // observed form-entry focus. Only the original focus owner is reset during
    // discovery. A directly seeded intermediate button is never evidence.
    fn focus001_cont012_native_route_in_form(
        window: &TvWindow,
        form: &adw::Dialog,
        target: &gtk::Widget,
    ) {
        let mut stack = vec![form.clone().upcast::<gtk::Widget>()];
        let mut controls = Vec::<gtk::Widget>::new();
        while let Some(widget) = stack.pop() {
            if widget.is_mapped() && widget.is_sensitive() && widget.is_focusable() {
                controls.push(widget.clone());
            }
            let mut child = widget.first_child();
            while let Some(item) = child {
                child = item.next_sibling();
                stack.push(item);
            }
        }
        let identify = |owner: &gtk::Widget| {
            controls
                .iter()
                .position(|control| control == owner)
                .or_else(|| {
                    controls
                        .iter()
                        .position(|control| TvWindow::is_within(owner, control))
                })
        };
        let initial_owner = window
            .focused_widget()
            .expect("RED: native guide form has no actual initial focus owner");
        assert!(
            initial_owner.is_mapped() && TvWindow::is_within(&initial_owner, form.upcast_ref()),
            "RED: initial guide form focus is not mapped inside the modal"
        );
        let start = identify(&initial_owner)
            .expect("RED: genuine guide form focus owner is not a named mapped control");
        let goal = controls
            .iter()
            .position(|control| control == target)
            .expect("RED: requested guide target not among mapped controls");
        let mut paths = vec![None::<Vec<&'static str>>; controls.len()];
        paths[start] = Some(vec![]);
        let mut queue = std::collections::VecDeque::from([start]);
        let mut observed_arrows = 0usize;
        while paths[goal].is_none() {
            let Some(index) = queue.pop_front() else {
                break;
            };
            let prefix = paths[index].clone().expect("already discovered path");
            for key in ["Up", "Down", "Left", "Right"] {
                assert!(
                    initial_owner.grab_focus(),
                    "RED: cannot reset the actual native initial form owner"
                );
                for step in &prefix {
                    native_x11_key(step);
                }
                let prior = window.focused_widget().and_then(|w| identify(&w));
                assert_eq!(
                    prior,
                    Some(index),
                    "RED: genuine native form prefix does not reproduce its previously observed focus"
                );
                native_x11_key(key);
                observed_arrows += 1;
                if let Some(next) = window.focused_widget().and_then(|w| identify(&w)) {
                    if paths[next].is_none() {
                        let mut candidate = prefix.clone();
                        candidate.push(key);
                        paths[next] = Some(candidate);
                        queue.push_back(next);
                    }
                    if next == goal {
                        break;
                    }
                } else if let Some(other) = window.focused_widget() {
                    eprintln!(
                        "FOCUS001 cont012 unclassified_native_focus from={} arrow={} owner={} inside_form={} mapped={} sensitive={}",
                        index, key, other.type_().name(),
                        TvWindow::is_within(&other, form.upcast_ref()),
                        other.is_mapped(), other.is_sensitive()
                    );
                }
            }
        }
        eprintln!(
            "FOCUS001 cont012 authentic_form_graph start_type={} controls={} native_arrows={} discovered={} target_type={} path={:?}",
            initial_owner.type_().name(),
            controls.len(),
            observed_arrows,
            paths.iter().filter(|path| path.is_some()).count(),
            target.type_().name(),
            paths[goal]
        );
        let route = paths[goal].as_ref().expect(
            "RED: required guide control unreachable by authentic connected native D-pad graph",
        );
        assert!(
            initial_owner.grab_focus(),
            "RED: cannot reset native modal entry before definitive target replay"
        );
        for key in route {
            native_x11_key(key);
        }
        assert_owner(window, target, "guide form real native target route");
    }

    // Discover manager directionality with physical arrows, then replay the
    // complete path from the ACTUAL focused manager entry; seeded observations
    // only construct the graph, never count as successful activation.
    fn focus001_cont012_reach_named_guide_button(
        window: &TvWindow,
        guide_manager: &adw::Dialog,
        button_name: &str,
    ) {
        let labels = [
            "Add URL",
            "Choose Local XMLTV",
            "Close",
            "Use as Shared Default",
            "Refresh",
            "Edit",
            "Remove",
        ];
        let controls: Vec<gtk::Widget> = labels
            .iter()
            .map(|label| focus001_button_in(guide_manager, label).upcast::<gtk::Widget>())
            .filter(|control| control.is_mapped() && control.is_sensitive())
            .collect();
        let goal = focus001_button_in(guide_manager, button_name).upcast::<gtk::Widget>();
        let identify = |owner: &gtk::Widget| {
            controls
                .iter()
                .position(|control| TvWindow::is_within(owner, control))
        };
        let start_owner = window
            .focused_widget()
            .expect("native guide manager entry focus");
        let start = identify(&start_owner).expect("real guide entry is mapped");
        let destination = controls
            .iter()
            .position(|control| *control == goal)
            .expect("enabled named guide action");
        let mut edges = vec![Vec::<(usize, &'static str)>::new(); controls.len()];
        for (i, control) in controls.iter().enumerate() {
            for direction in ["Up", "Down", "Left", "Right"] {
                assert!(control.grab_focus(), "only map an existing enabled control");
                native_x11_key(direction);
                if let Some(next) = window.focused_widget().and_then(|w| identify(&w)) {
                    if next != i {
                        edges[i].push((next, direction));
                    }
                }
            }
        }
        let mut previous = vec![None::<(usize, &'static str)>; controls.len()];
        let mut reached = vec![false; controls.len()];
        let mut queue = std::collections::VecDeque::from([start]);
        reached[start] = true;
        while let Some(i) = queue.pop_front() {
            for &(j, direction) in &edges[i] {
                if !reached[j] {
                    reached[j] = true;
                    previous[j] = Some((i, direction));
                    queue.push_back(j);
                }
            }
        }
        eprintln!(
            "FOCUS001 cont012 guide manager action={button_name} reachable={} enabled_controls={}",
            reached[destination],
            controls.len()
        );
        assert!(
            reached[destination],
            "RED: guide Edit is unreachable by genuine D-pad"
        );
        let mut route = Vec::new();
        let mut cursor = destination;
        while cursor != start {
            let (prev, direction) = previous[cursor].expect("connected guide path");
            route.push((prev, cursor, direction));
            cursor = prev;
        }
        route.reverse();
        assert!(
            controls[start].grab_focus(),
            "restore authentic reached guide entry"
        );
        for (from, to, direction) in &route {
            assert_owner(window, &controls[*from], "real guide path current");
            native_x11_key(direction);
            assert_owner(window, &controls[*to], "real guide path destination");
        }
        assert_owner(window, &goal, "real guide connected target");
    }

    // AG02: The public application is unchanged. Start with either a synthetic
    // enabled or disabled disposable guide to verify *both* remote toggles.
    // Fixture seeding is not counted as a native user action.
    fn focus001_cont012_guide_enabled_apply_native(width: i32, initial_enabled: bool) {
        crate::epg::desktop::focus001_seed_disposable_guides(1);
        if !initial_enabled {
            let registry = crate::epg::sources::GuideSourceRegistry::new(
                crate::streams::storage::Storage::new(),
            );
            let guide = registry
                .list()
                .expect("read synthetic guide fixture")
                .into_iter()
                .find(|source| source.name == "FOCUS synthetic guide 0")
                .expect("synthetic source exists");
            registry
                .set_enabled(&guide.id, false)
                .expect("seed synthetic disabled guide without network");
        }
        let (_app, window, scratch) = focus001_manager_fixture(1);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let source_manager = focus001_recovery_open_manager_native(&window);
        assert_owner(
            &window,
            focus001_button_in(&source_manager, "Add Playlist").upcast_ref(),
            "actual source manager entry",
        );
        native_x11_key("Right");
        assert_owner(
            &window,
            focus001_button_in(&source_manager, "Programme Guides").upcast_ref(),
            "native guide portal",
        );
        native_x11_key("Return");
        let guide_manager = focus001_dialog_after(&window, Some(&source_manager));
        assert_owner(
            &window,
            focus001_button_in(&guide_manager, "Add URL").upcast_ref(),
            "guide native start",
        );
        focus001_cont012_reach_named_guide_button(&window, &guide_manager, "Edit");
        native_x11_key("Return");
        let form = focus001_dialog_after(&window, Some(&guide_manager));
        let enabled = TvWindow::find_descendant(form.upcast_ref(), &|widget| {
            widget
                .downcast_ref::<gtk::CheckButton>()
                .is_some_and(|check| check.label().as_deref() == Some("Enabled"))
        })
        .expect("mapped named Enabled guide setting")
        .downcast::<gtk::CheckButton>()
        .expect("Enabled GtkCheckButton");
        let apply = focus001_button_in(&form, "Apply");
        assert_eq!(
            enabled.is_active(),
            initial_enabled,
            "expected synthetic guide initial state"
        );
        focus001_cont012_native_route_in_form(&window, &form, enabled.upcast_ref());
        native_x11_key("Return");
        assert_eq!(
            enabled.is_active(),
            !initial_enabled,
            "native OK must toggle Enabled"
        );
        focus001_cont012_native_route_in_form(&window, &form, apply.upcast_ref());
        native_x11_key("Return");
        let registry =
            crate::epg::sources::GuideSourceRegistry::new(crate::streams::storage::Storage::new());
        let mut persisted = false;
        for _ in 0..60 {
            settle();
            persisted = registry
                .list()
                .expect("disposable guide registry")
                .iter()
                .any(|source| {
                    source.name == "FOCUS synthetic guide 0" && source.enabled == !initial_enabled
                });
            if persisted {
                break;
            }
        }
        let current = window.visible_dialog();
        let owner = window.focused_widget();
        let focus_valid = owner.as_ref().is_some_and(|widget| {
            widget.is_mapped()
                && widget.is_sensitive()
                && TvWindow::is_within(widget, guide_manager.upcast_ref())
        });
        eprintln!(
            "FOCUS001 cont012 guide_enabled initial_enabled={initial_enabled} expected_enabled={} native_apply=true persisted={persisted} viewport={} manager_visible={} manager_focus={} owner_type={:?}",
            !initial_enabled, width,
            current == Some(guide_manager.clone()),
            focus_valid,
            owner.as_ref().map(|widget| widget.type_().name())
        );
        window.close();
        std::fs::remove_dir_all(scratch).expect("cleanup synthetic registry fixture");
        assert!(
            persisted,
            "RED: native guide Enabled Apply did not persist requested state to SQLite"
        );
        assert!(
            current == Some(guide_manager),
            "RED: guide Apply must dismiss editor back to guide manager"
        );
        assert!(
            focus_valid,
            "RED: guide Apply lost mapped guide manager focus"
        );
    }

    // AG04: Do not replace the OSK with injected text or direct Entry.set_text().
    // The only source mutation below is a synthetic disposable fixture.
    #[test]
    #[ignore = "AG04: real source-to-guide Edit, native OSK key, Apply and SQLite name persistence at 640x480"]
    fn gtk_focus001_cont012_guide_edit_osk_save_native_compact() {
        crate::epg::desktop::focus001_seed_disposable_guides(1);
        let (_app, window, scratch) = focus001_manager_fixture(1);
        window.set_default_size(640, 480);
        let source_manager = focus001_recovery_open_manager_native(&window);
        assert_owner(
            &window,
            focus001_button_in(&source_manager, "Add Playlist").upcast_ref(),
            "source manager entry before guide edit",
        );
        native_x11_key("Right");
        assert_owner(
            &window,
            focus001_button_in(&source_manager, "Programme Guides").upcast_ref(),
            "native source-to-guide portal",
        );
        native_x11_key("Return");
        let guide_manager = focus001_dialog_after(&window, Some(&source_manager));
        assert_owner(
            &window,
            focus001_button_in(&guide_manager, "Add URL").upcast_ref(),
            "guide manager true entry",
        );
        for arrow in ["Down", "Right", "Right"] {
            native_x11_key(arrow);
        }
        assert_owner(
            &window,
            focus001_button_in(&guide_manager, "Edit").upcast_ref(),
            "native connected guide Edit",
        );
        native_x11_key("Return");
        let form = focus001_dialog_after(&window, Some(&guide_manager));
        let entry =
            TvWindow::find_descendant(form.upcast_ref(), &|widget| widget.is::<gtk::Entry>())
                .expect("real guide Name entry")
                .downcast::<gtk::Entry>()
                .expect("mapped GTK Entry");
        let initial_name = entry.text().to_string();
        assert_eq!(initial_name, "FOCUS synthetic guide 0");
        let keyboard = entry
            .next_sibling()
            .expect("guide Name editor must have keyboard launcher sibling")
            .downcast::<gtk::Button>()
            .expect("mapped guide OSK button");
        assert!(keyboard.is_mapped() && keyboard.is_sensitive());
        // The survey helper replays a physical XTest path from the focused form
        // entry; the survey's individual grab_focus calls are NOT reachability.
        focus001_cont012_native_route_in_form(&window, &form, keyboard.upcast_ref());
        native_x11_key("Return");
        let osk = focus001_dialog_after(&window, Some(&form));
        let owner = window.focused_widget().expect("OSK must own focused key");
        assert!(owner.is_mapped() && TvWindow::is_within(&owner, osk.upcast_ref()));
        let q = focus001_button_in(&osk, "q");
        assert_owner(&window, q.upcast_ref(), "real OSK first focused letter");
        native_x11_key("Return");
        assert_ne!(
            entry.text().as_str(),
            initial_name,
            "native OSK key must edit guide name"
        );
        // Escape dismisses the OSK using native remote semantics; it cannot
        // be replaced by seeded Done.grab_focus() or direct dialog.close().
        native_x11_key("Escape");
        assert_eq!(
            window.visible_dialog(),
            Some(form.clone()),
            "OSK Escape returns guide Edit"
        );
        let edited_name = entry.text().to_string();
        focus001_cont012_native_route_in_form(
            &window,
            &form,
            focus001_button_in(&form, "Apply").upcast_ref(),
        );
        native_x11_key("Return");
        let registry =
            crate::epg::sources::GuideSourceRegistry::new(crate::streams::storage::Storage::new());
        let mut persisted = false;
        for _ in 0..60 {
            settle();
            persisted = registry
                .list()
                .expect("disposable guide read")
                .iter()
                .any(|source| source.name == edited_name);
            if persisted {
                break;
            }
        }
        let manager_active = window.visible_dialog() == Some(guide_manager.clone());
        let manager_focus = window.focused_widget().as_ref().is_some_and(|widget| {
            widget.is_mapped()
                && widget.is_sensitive()
                && TvWindow::is_within(widget, guide_manager.upcast_ref())
        });
        eprintln!("FOCUS001 cont012 AG04 native_osk_edit=true persisted={persisted} guide_manager_visible={manager_active} focus_return={manager_focus}");
        window.close();
        std::fs::remove_dir_all(scratch).expect("dispose guide OSK fixture");
        assert!(
            persisted,
            "RED: real OSK-entered guide Name was not saved to SQLite"
        );
        assert!(manager_active, "RED: guide Apply did not return to manager");
        assert!(
            manager_focus,
            "RED: native guide OSK Apply lost focus after async repaint"
        );
    }

    #[test]
    #[ignore = "AG02: native guide Enabled→disabled Apply and SQLite after async update, 640x480"]
    fn gtk_focus001_cont012_guide_disable_apply_native_compact() {
        focus001_cont012_guide_enabled_apply_native(640, true);
    }

    #[test]
    #[ignore = "AG02: native guide Disabled→enabled Apply and SQLite after async update, 1280x800"]
    fn gtk_focus001_cont012_guide_enable_apply_native_tv() {
        focus001_cont012_guide_enabled_apply_native(1280, false);
    }

    // Continuation-011 B07: connected Search -> populated Source DropDown
    // native D-pad route. This is the actual selector portal and selection;
    // search/scroll and Clear through *native* direction are separate gates.
    fn focus001_cont011_source_facet_connected(width: i32) {
        let (_app, window, scratch) = focus001_manager_fixture(3);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        settle();
        let toolbar = window.imp().streams_view.toolbar_focus_targets();
        let search = toolbar[0]
            .clone()
            .downcast::<gtk::SearchEntry>()
            .expect("real My Channels SearchEntry");
        let source = toolbar[1]
            .clone()
            .downcast::<gtk::DropDown>()
            .expect("real populated Source DropDown");
        for _ in 0..45 {
            settle();
            if source.model().is_some_and(|m| m.n_items() >= 4) {
                break;
            }
        }
        assert!(source.model().is_some_and(|m| m.n_items() >= 4));
        assert_eq!(source.selected(), 0, "fixture starts with all sources");
        assert_native_input_delivery(search.upcast_ref());
        search.set_text("");
        settle();
        let usable = |i: usize| {
            toolbar[i].is_mapped() && toolbar[i].is_sensitive() && toolbar[i].is_visible()
        };
        assert!(
            usable(0) && usable(1),
            "source selector must be enabled and visible"
        );
        let mut edges = vec![Vec::<(usize, &'static str)>::new(); toolbar.len()];
        for i in 0..toolbar.len() {
            if !usable(i) {
                continue;
            }
            for key in ["Up", "Down", "Left", "Right"] {
                assert!(toolbar[i].grab_focus(), "diagnostic origin mapped");
                native_x11_key(key);
                if let Some(j) = window.focused_widget().and_then(|owner| {
                    toolbar.iter().enumerate().find_map(|(j, item)| {
                        (usable(j) && TvWindow::is_within(&owner, item)).then_some(j)
                    })
                }) {
                    if j != i {
                        edges[i].push((j, key));
                    }
                }
            }
        }
        let mut parent = vec![None::<(usize, &'static str)>; toolbar.len()];
        let mut seen = vec![false; toolbar.len()];
        let mut queue = std::collections::VecDeque::from([0usize]);
        seen[0] = true;
        while let Some(i) = queue.pop_front() {
            for &(j, key) in &edges[i] {
                if !seen[j] {
                    seen[j] = true;
                    parent[j] = Some((i, key));
                    queue.push_back(j);
                }
            }
        }
        let mut route = Vec::<(usize, usize, &'static str)>::new();
        if seen[1] {
            let mut cursor = 1usize;
            while cursor != 0 {
                let (from, key) = parent[cursor].expect("native route predecessor");
                route.push((from, cursor, key));
                cursor = from;
            }
            route.reverse();
            assert!(search.grab_focus(), "real Search route origin still mapped");
            for &(from, to, key) in &route {
                assert_owner(&window, &toolbar[from], "source portal native origin");
                native_x11_key(key);
                assert_owner(&window, &toolbar[to], "source portal native destination");
            }
            native_x11_key("space");
            native_x11_key("Down");
            native_x11_key("Return");
        }
        let mut selected = source.selected();
        for _ in 0..35 {
            settle();
            selected = source.selected();
            if selected != 0 {
                break;
            }
        }
        let owner_is_source = window.focused_widget().is_some_and(|owner| {
            owner.is_mapped()
                && owner.is_sensitive()
                && TvWindow::is_within(&owner, source.upcast_ref())
        });
        eprintln!("FOCUS001 cont011 source_facet viewport={width} connected={} path={:?} selected={selected} owner_source={owner_is_source}",
            seen[1], route.iter().map(|(_, _, key)| *key).collect::<Vec<_>>());
        window.close();
        std::fs::remove_dir_all(scratch).expect("destroy synthetic facet sources");
        assert!(
            seen[1],
            "RED: Source facet not reachable from Search by physical arrow graph"
        );
        assert!(
            selected > 0,
            "RED: native source facet OK failed to select synthetic source"
        );
        assert!(
            owner_is_source,
            "RED: source facet selection orphaned native focus"
        );
    }

    #[test]
    #[ignore = "native connected populated Source selector portal compact"]
    fn gtk_focus001_cont011_source_facet_portal_compact() {
        focus001_cont011_source_facet_connected(640);
    }

    #[test]
    #[ignore = "native connected populated Source selector portal TV"]
    fn gtk_focus001_cont011_source_facet_portal_tv() {
        focus001_cont011_source_facet_connected(1280);
    }

    // Continuation-011: a real Search-origin directional route to each
    // populated persisted facet. Survey focus seeding is diagnostic only;
    // the claimed path must be replayed with native XTest arrows. Following
    // portal activation the case verifies the actual GtkDropDown selection
    // and mapped focus. This does NOT claim full OSK search/clear/intersection.
    fn focus001_cont011_rich_facet_connected(facet_index: usize, width: i32) {
        assert!([1, 2, 4, 5].contains(&facet_index));
        let (_app, window) = mapped_window("streams");
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let scratch = std::env::temp_dir().join(format!(
            "focus001-cont011-rich-{}-{}",
            facet_index,
            uuid::Uuid::new_v4()
        ));
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&scratch)
            .expect("temporary private synthetic source fixture");
        let file = scratch.join("rich.sqlite");
        crate::streams::view::TvStreamsView::focus001_seed_rich_facet_sources(file.clone());
        let view = &window.imp().streams_view;
        let ready = view.focus001_attach_disposable_service(file);
        for _ in 0..75 {
            settle();
            if ready.get() {
                break;
            }
        }
        assert!(ready.get(), "synthetic facet metadata did not load");
        let toolbar = view.toolbar_focus_targets();
        assert_native_input_delivery(&toolbar[0]);
        let source = toolbar[facet_index]
            .clone()
            .downcast::<gtk::DropDown>()
            .expect("populated real GTK facet DropDown");
        for _ in 0..60 {
            settle();
            if source.model().is_some_and(|m| m.n_items() >= 3) {
                break;
            }
        }
        assert!(source.model().is_some_and(|m| m.n_items() >= 3));
        assert_eq!(source.selected(), 0);
        if let Some(entry) = toolbar[0].downcast_ref::<gtk::SearchEntry>() {
            entry.set_text("");
            settle();
        }
        let usable = |i: usize| {
            toolbar[i].is_mapped() && toolbar[i].is_visible() && toolbar[i].is_sensitive()
        };
        assert!(usable(0) && usable(facet_index));
        let mut edges = vec![Vec::<(usize, &'static str)>::new(); toolbar.len()];
        for i in 0..toolbar.len() {
            if !usable(i) {
                continue;
            }
            for key in ["Up", "Down", "Left", "Right"] {
                assert!(toolbar[i].grab_focus(), "mapped survey toolbar origin");
                native_x11_key(key);
                if let Some(j) = window.focused_widget().and_then(|owner| {
                    toolbar.iter().enumerate().find_map(|(j, item)| {
                        (usable(j) && TvWindow::is_within(&owner, item)).then_some(j)
                    })
                }) {
                    if j != i {
                        edges[i].push((j, key));
                    }
                }
            }
        }
        let mut parents = vec![None::<(usize, &'static str)>; toolbar.len()];
        let mut visited = vec![false; toolbar.len()];
        let mut queue = std::collections::VecDeque::from([0usize]);
        visited[0] = true;
        while let Some(i) = queue.pop_front() {
            for &(j, key) in &edges[i] {
                if !visited[j] {
                    visited[j] = true;
                    parents[j] = Some((i, key));
                    queue.push_back(j);
                }
            }
        }
        let mut route = Vec::<(usize, usize, &'static str)>::new();
        if visited[facet_index] {
            let mut cursor = facet_index;
            while cursor != 0 {
                let (from, key) = parents[cursor].expect("mapped native path parent");
                route.push((from, cursor, key));
                cursor = from;
            }
            route.reverse();
            assert!(toolbar[0].grab_focus(), "authentic Search origin retained");
            for &(from, to, key) in &route {
                assert_owner(&window, &toolbar[from], "facet route physical origin");
                native_x11_key(key);
                assert_owner(&window, &toolbar[to], "facet route physical destination");
            }
            native_x11_key("space");
            native_x11_key("Down");
            native_x11_key("Return");
        }
        let mut selected = source.selected();
        for _ in 0..35 {
            settle();
            selected = source.selected();
            if selected != 0 {
                break;
            }
        }
        let still_mapped = window.focused_widget().is_some_and(|owner| {
            owner.is_mapped()
                && owner.is_sensitive()
                && TvWindow::is_within(&owner, source.upcast_ref())
        });
        eprintln!("FOCUS001 cont011 rich_facet viewport={width} index={facet_index} connected={} path={:?} selected={selected} owner_source={still_mapped}",
            visited[facet_index],route.iter().map(|(_,_,key)|*key).collect::<Vec<_>>());
        window.close();
        std::fs::remove_dir_all(scratch).expect("delete disposable rich metadata fixture");
        assert!(
            visited[facet_index],
            "RED: populated facet not reachable from Search by native arrows"
        );
        assert!(
            selected > 0,
            "RED: native DropDown selection failed after connected portal"
        );
        assert!(
            still_mapped,
            "RED: selected facet loses native focus after async update"
        );
    }

    #[test]
    #[ignore = "real native populated Source facet portal at 640x480"]
    fn gtk_focus001_cont011_rich_source_facet_compact() {
        focus001_cont011_rich_facet_connected(1, 640);
    }
    #[test]
    #[ignore = "real native populated Source facet portal at 1280x800"]
    fn gtk_focus001_cont011_rich_source_facet_tv() {
        focus001_cont011_rich_facet_connected(1, 1280);
    }
    #[test]
    #[ignore = "real native populated Group facet portal at 640x480"]
    fn gtk_focus001_cont011_rich_group_facet_compact() {
        focus001_cont011_rich_facet_connected(2, 640);
    }
    #[test]
    #[ignore = "real native populated Group facet portal at 1280x800"]
    fn gtk_focus001_cont011_rich_group_facet_tv() {
        focus001_cont011_rich_facet_connected(2, 1280);
    }
    #[test]
    #[ignore = "real native populated Language facet portal at 640x480"]
    fn gtk_focus001_cont011_rich_language_facet_compact() {
        focus001_cont011_rich_facet_connected(4, 640);
    }
    #[test]
    #[ignore = "real native populated Language facet portal at 1280x800"]
    fn gtk_focus001_cont011_rich_language_facet_tv() {
        focus001_cont011_rich_facet_connected(4, 1280);
    }
    #[test]
    #[ignore = "real native populated Country facet portal at 640x480"]
    fn gtk_focus001_cont011_rich_country_facet_compact() {
        focus001_cont011_rich_facet_connected(5, 640);
    }
    #[test]
    #[ignore = "real native populated Country facet portal at 1280x800"]
    fn gtk_focus001_cont011_rich_country_facet_tv() {
        focus001_cont011_rich_facet_connected(5, 1280);
    }

    // Continuation-012: one connected actual XTest journey across four
    // *intersecting* persistent SQLite facets. A separate diagnostic survey
    // discovers directional edges; the asserted route itself never uses a
    // seeded intermediate focus, Tab, mouse or direct selection callbacks.
    fn focus001_cont012_intersect_and_clear(width: i32, sort_and_empty_reload: bool) {
        use std::collections::{HashSet, VecDeque};
        let (_app, window) = mapped_window("streams");
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let scratch =
            std::env::temp_dir().join(format!("focus001-cont012-facets-{}", uuid::Uuid::new_v4()));
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&scratch)
            .expect("private disposable multi-facet DB fixture");
        let database = scratch.join("intersection.sqlite");
        crate::streams::view::TvStreamsView::focus001_seed_rich_facet_sources(database.clone());
        let view = &window.imp().streams_view;
        let ready = view.focus001_attach_disposable_service(database);
        for _ in 0..80 {
            settle();
            if ready.get()
                && view.toolbar_focus_targets()[1]
                    .clone()
                    .downcast::<gtk::DropDown>()
                    .is_ok_and(|d| d.model().is_some_and(|m| m.n_items() >= 4))
            {
                break;
            }
        }
        assert!(ready.get(), "synthetic SQLite source fixture unavailable");
        let controls = view.toolbar_focus_targets();
        assert_native_input_delivery(&controls[0]);
        assert_eq!(view.focus001_test_result_snapshot().1, 12);
        let usable = |i: usize| {
            controls[i].is_mapped() && controls[i].is_visible() && controls[i].is_sensitive()
        };
        // Diagnostic-only focus sampling. No seeded focus is accepted as a
        // valid traversal: the actual compound journey is entirely replayed
        // via physical XTest keys from the real Search entry.
        let mut edges = vec![Vec::<(usize, &'static str)>::new(); controls.len()];
        for i in 0..controls.len() {
            if !usable(i) {
                continue;
            }
            for direction in ["Up", "Down", "Left", "Right"] {
                assert!(controls[i].grab_focus(), "diagnostic control unavailable");
                native_x11_key(direction);
                let next = window.focused_widget().and_then(|owner| {
                    controls.iter().enumerate().find_map(|(j, target)| {
                        (j != i && usable(j) && TvWindow::is_within(&owner, target)).then_some(j)
                    })
                });
                if let Some(j) = next {
                    edges[i].push((j, direction));
                }
            }
        }
        // A reset to initial Search is setup, not part of the asserted route.
        assert!(controls[0].grab_focus(), "real Search input entry");
        assert_owner(&window, &controls[0], "intersection start Search");
        let mut current = 0usize;
        let navigate_to = |destination: usize, current: &mut usize| {
            let mut parents = vec![None::<(usize, &'static str)>; controls.len()];
            let mut seen = vec![false; controls.len()];
            let mut q = VecDeque::from([*current]);
            seen[*current] = true;
            while let Some(i) = q.pop_front() {
                for &(j, direction) in &edges[i] {
                    if !seen[j] {
                        seen[j] = true;
                        parents[j] = Some((i, direction));
                        q.push_back(j);
                    }
                }
            }
            assert!(
                seen[destination],
                "RED: physical D-pad graph cannot reach compound facet {destination} from {}",
                *current
            );
            let mut path = Vec::new();
            let mut cursor = destination;
            while cursor != *current {
                let (prev, direction) = parents[cursor].expect("native route parent");
                path.push((prev, cursor, direction));
                cursor = prev;
            }
            path.reverse();
            for (from, to, direction) in path {
                assert_eq!(*current, from);
                assert_owner(&window, &controls[from], "native intersect edge origin");
                native_x11_key(direction);
                assert_owner(&window, &controls[to], "native intersect edge target");
                *current = to;
            }
        };
        // Source 4/12; Group News 2/4; Language de 2/2; Country DE 2/2.
        // Every one of these is selected by actual keys on actual DropDown.
        for (i, expected) in [(1usize, 4u32), (2, 2), (4, 2), (5, 2)] {
            navigate_to(i, &mut current);
            let dropdown = controls[i]
                .clone()
                .downcast::<gtk::DropDown>()
                .expect("real native persisted facet dropdown");
            assert_eq!(
                dropdown.selected(),
                0,
                "unexpected selected facet from previous mutation"
            );
            let before = view.focus001_test_result_snapshot().0;
            native_x11_key("space");
            native_x11_key("Down");
            native_x11_key("Return");
            let mut evidence = view.focus001_test_result_snapshot();
            for _ in 0..65 {
                settle();
                evidence = view.focus001_test_result_snapshot();
                if evidence.0 > before && evidence.1 == expected {
                    break;
                }
            }
            eprintln!("FOCUS001 cont012 intersection width={width} selected_facet={i} value={} expected_count={expected} actual_count={} generation={} sources={:?}",
                dropdown.selected(), evidence.1, evidence.0,
                evidence.2.iter().map(|(source, _)| source).collect::<HashSet<_>>());
            assert!(
                dropdown.selected() > 0,
                "RED: native XTest failed to select the actual facet {i}"
            );
            assert!(
                evidence.0 > before,
                "RED: real selected facet {i} did not cause an async query"
            );
            assert_eq!(
                evidence.1, expected,
                "RED: intersecting filters did not narrow to expected fixture count"
            );
            assert_eq!(
                evidence.2.len(),
                expected as usize,
                "RED: async filtered source-qualified rows not rebound"
            );
            let sources: HashSet<_> = evidence.2.iter().map(|(id, _)| id).collect();
            assert_eq!(
                sources.len(),
                1,
                "RED: source-filter intersection leaked another playlist"
            );
            assert_owner(
                &window,
                &controls[i],
                "physical facet remains the focus owner after async result repaint",
            );
            current = i;
        }
        if sort_and_empty_reload {
            // Further F05 subassertions, still through this connected D-pad
            // session: Sort is a separate SQL query parameter and typing a
            // never-matching Search string triggers a real debounced async
            // reload while the four facets remain selected.
            navigate_to(3, &mut current);
            let sort = controls[3]
                .clone()
                .downcast::<gtk::DropDown>()
                .expect("mapped Sort DropDown");
            assert_eq!(sort.selected(), 0);
            let before_sort = view.focus001_test_result_snapshot().0;
            native_x11_key("space");
            native_x11_key("Down");
            native_x11_key("Return");
            let mut after_sort = view.focus001_test_result_snapshot();
            for _ in 0..80 {
                settle();
                after_sort = view.focus001_test_result_snapshot();
                if after_sort.0 > before_sort && after_sort.1 == 2 {
                    break;
                }
            }
            assert_eq!(
                sort.selected(),
                1,
                "RED: real keys did not select Name sort"
            );
            assert!(
                after_sort.0 > before_sort,
                "RED: sorting did not requery SQLite"
            );
            assert_eq!(after_sort.1, 2, "RED: sort lost four-facet intersection");
            assert_owner(&window, &controls[3], "native Sort focus survives requery");
            current = 3;

            navigate_to(0, &mut current);
            let before_search = view.focus001_test_result_snapshot().0;
            native_x11_key("z");
            let mut empty = view.focus001_test_result_snapshot();
            for _ in 0..100 {
                settle();
                empty = view.focus001_test_result_snapshot();
                if empty.0 > before_search && empty.1 == 0 {
                    break;
                }
            }
            assert!(
                empty.0 > before_search,
                "RED: native Search typing did not trigger the debounced async query"
            );
            assert_eq!(
                empty.1, 0,
                "RED: Search query did not eliminate intersecting results"
            );
            assert_owner(
                &window,
                &controls[0],
                "real Search keeps focus on async empty-result transition",
            );
            // Empty result lists legitimately reflow the geometry. Restore
            // the *same* four-facet result context via native Backspace, not
            // a seeded focus/reset or a stale pre-empty directed graph.
            let before_restore = empty.0;
            native_x11_key("BackSpace");
            let mut restored = view.focus001_test_result_snapshot();
            for _ in 0..100 {
                settle();
                restored = view.focus001_test_result_snapshot();
                if restored.0 > before_restore && restored.1 == 2 {
                    break;
                }
            }
            assert!(
                restored.0 > before_restore,
                "RED: Backspace did not trigger async Search restoration"
            );
            assert_eq!(
                restored.1, 2,
                "RED: Backspace failed to restore intersecting filtered rows"
            );
            assert_owner(
                &window,
                &controls[0],
                "Search remains owner after genuine Backspace async restore",
            );
            eprintln!(
                "FOCUS001 cont012 F05 real_backspace width={width} count={} generation={}",
                restored.1, restored.0
            );
            eprintln!("FOCUS001 cont012 F05 sort_search width={width} sorted_count={} empty_count={} generation={} native_z=true",
                after_sort.1, empty.1, empty.0);
            // This phase has established native Sort+empty/reload behaviour.
            // A changed Sort label reflows the toolbar, invalidating the
            // pre-state focus map. Clear after that reflow remains unverified,
            // not a PASS or product RED from a stale graph.
            window.close();
            std::fs::remove_dir_all(scratch).expect("remove synthetic DB and profile");
            return;
        }
        navigate_to(7, &mut current);
        let before_clear = view.focus001_test_result_snapshot().0;
        native_x11_key("Return");
        let mut after_clear = view.focus001_test_result_snapshot();
        for _ in 0..80 {
            settle();
            after_clear = view.focus001_test_result_snapshot();
            if after_clear.0 > before_clear && after_clear.1 == 12 {
                break;
            }
        }
        eprintln!(
            "FOCUS001 cont012 clear width={width} count={} source_ids={} generation={}",
            after_clear.1,
            after_clear
                .2
                .iter()
                .map(|(s, _)| s)
                .collect::<HashSet<_>>()
                .len(),
            after_clear.0
        );
        assert!(
            after_clear.0 > before_clear,
            "RED: Clear Filters did not trigger the actual async query"
        );
        assert_eq!(
            after_clear.1, 12,
            "RED: Clear Filters did not restore all rows"
        );
        assert_eq!(
            after_clear
                .2
                .iter()
                .map(|(s, _)| s)
                .collect::<HashSet<_>>()
                .len(),
            3,
            "RED: Clear Filters left the source partition narrowed"
        );
        for i in [1usize, 2, 4, 5] {
            assert_eq!(
                controls[i]
                    .clone()
                    .downcast::<gtk::DropDown>()
                    .expect("facet after Clear Filters")
                    .selected(),
                0,
                "RED: filter state did not reset on Clear Filters"
            );
        }
        assert_owner(&window, &controls[7], "Clear Filters retains mapped focus");
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove synthetic DB and profile");
    }

    #[test]
    #[ignore = "native compact 4-facet combined SQLite intersection + Clear Filters"]
    fn gtk_focus001_cont012_four_facet_intersection_clear_compact() {
        focus001_cont012_intersect_and_clear(640, false);
    }

    #[test]
    #[ignore = "native TV 4-facet combined SQLite intersection + Clear Filters"]
    fn gtk_focus001_cont012_four_facet_intersection_clear_tv() {
        focus001_cont012_intersect_and_clear(1280, false);
    }

    #[test]
    #[ignore = "native compact four-facet + Sort + typed async empty/reload and Clear"]
    fn gtk_focus001_cont012_four_facet_sort_async_empty_restore_compact() {
        focus001_cont012_intersect_and_clear(640, true);
    }

    #[test]
    #[ignore = "native TV four-facet + Sort + typed async empty/reload and Clear"]
    fn gtk_focus001_cont012_four_facet_sort_async_empty_restore_tv() {
        focus001_cont012_intersect_and_clear(1280, true);
    }

    // Continuation-011: a single native graph survey across *every* currently
    // enabled source-manager row action, in a disposable three-source manager.
    // Each survey seed is diagnostic only: a path is counted only if replayed
    // from the manager's native-entry widget with real XTest arrows.
    // This tests reachability, never simulates a destructive action/commit.
    fn focus001_cont011_source_manager_action_reachability(width: i32) {
        let (_app, window, scratch) = focus001_manager_fixture(3);
        window.set_default_size(width, if width == 640 { 480 } else { 800 });
        let manager = focus001_recovery_open_manager_native(&window);
        let list =
            TvWindow::find_descendant(manager.upcast_ref(), &|widget| widget.is::<gtk::ListBox>())
                .expect("source manager mapped ListBox")
                .downcast::<gtk::ListBox>()
                .expect("real source manager GtkListBox");
        let mut controls = Vec::<gtk::Widget>::new();
        let mut stack = vec![manager.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = stack.pop() {
            if widget.is_mapped()
                && widget.is_visible()
                && widget.is_sensitive()
                && widget.is_focusable()
                && (widget.is::<gtk::Button>() || widget.is::<gtk::Switch>())
            {
                controls.push(widget.clone());
            }
            let mut child = widget.first_child();
            while let Some(item) = child {
                child = item.next_sibling();
                stack.push(item);
            }
        }
        let identify = |focused: &gtk::Widget| {
            controls
                .iter()
                .enumerate()
                .find_map(|(index, w)| TvWindow::is_within(focused, w).then_some(index))
        };
        let native_entry = window
            .focused_widget()
            .expect("native source manager entry focus");
        let start = identify(&native_entry).expect("manager entry is a mapped action");
        let mut targets = Vec::<(String, usize)>::new();
        for row_index in 0..3 {
            let row = list
                .row_at_index(row_index)
                .expect("three source manager rows");
            for label in [
                "Refresh", "Defaults", "Rename", "Replace", "Delete", "↑", "↓",
            ] {
                if let Some(widget) = TvWindow::find_descendant(row.upcast_ref(), &|w| {
                    w.downcast_ref::<gtk::Button>().is_some_and(|b| {
                        b.label().as_deref() == Some(label) && b.is_mapped() && b.is_sensitive()
                    })
                }) {
                    let id = controls
                        .iter()
                        .position(|w| w == &widget)
                        .expect("enabled action included in control graph");
                    targets.push((format!("row{row_index}:{label}"), id));
                }
            }
        }
        assert!(
            targets.len() >= 17,
            "expected all enabled upper/lower actions across 3 sources"
        );
        let mut edges = vec![Vec::<(usize, &'static str)>::new(); controls.len()];
        for i in 0..controls.len() {
            for key in ["Up", "Down", "Left", "Right"] {
                if controls[i].is_mapped() && controls[i].grab_focus() {
                    native_x11_key(key);
                    if let Some(next) = window.focused_widget().and_then(|focus| identify(&focus)) {
                        if i != next {
                            edges[i].push((next, key));
                        }
                    }
                }
            }
        }
        let mut parent = vec![None::<(usize, &'static str)>; controls.len()];
        let mut visited = vec![false; controls.len()];
        visited[start] = true;
        let mut queue = std::collections::VecDeque::from([start]);
        while let Some(i) = queue.pop_front() {
            for &(j, key) in &edges[i] {
                if !visited[j] {
                    visited[j] = true;
                    parent[j] = Some((i, key));
                    queue.push_back(j);
                }
            }
        }
        let mut unreachable = Vec::new();
        let mut reachable = 0;
        for (name, goal) in &targets {
            if !visited[*goal] {
                unreachable.push(name.clone());
                eprintln!("FOCUS001 manager_action viewport={width} action={name} reachable=false");
                continue;
            }
            let mut route = Vec::<(usize, usize, &'static str)>::new();
            let mut cursor = *goal;
            while cursor != start {
                let (previous, key) = parent[cursor].expect("BFS parent for reachable target");
                route.push((previous, cursor, key));
                cursor = previous;
            }
            route.reverse();
            assert!(
                controls[start].grab_focus(),
                "reestablish actual native-entry origin"
            );
            for &(from, to, key) in &route {
                assert_owner(
                    &window,
                    &controls[from],
                    "source manager graph route origin",
                );
                native_x11_key(key);
                assert_owner(
                    &window,
                    &controls[to],
                    "source manager graph route destination",
                );
            }
            reachable += 1;
            eprintln!(
                "FOCUS001 manager_action viewport={width} action={name} reachable=true path={:?}",
                route.iter().map(|(_, _, key)| *key).collect::<Vec<_>>()
            );
        }
        eprintln!("FOCUS001 manager_action SUMMARY viewport={width} enabled_actions={} connected_replayed={reachable} missing={unreachable:?} controls={} graph_edges={}",
            targets.len(), controls.len(), edges.iter().map(Vec::len).sum::<usize>());
        window.close();
        std::fs::remove_dir_all(scratch).expect("remove disposable source manager fixture");
        assert!(unreachable.is_empty(),
            "RED: enabled source-manager buttons unreachable by connected native D-pad arrows; mutation/persistence remains separately untested");
    }

    #[test]
    #[ignore = "all source-manager action reachability via mapped XTest at 640x480"]
    fn gtk_focus001_cont011_manager_all_action_reachability_compact() {
        focus001_cont011_source_manager_action_reachability(640);
    }

    #[test]
    #[ignore = "all source-manager action reachability via mapped XTest at 1280x800"]
    fn gtk_focus001_cont011_manager_all_action_reachability_tv() {
        focus001_cont011_source_manager_action_reachability(1280);
    }
}
