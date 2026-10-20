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
    ) -> bool {
        let controls = match page_name {
            "live" => self.imp().live_view.toolbar_focus_targets(),
            "mediathek" => self.imp().mediathek_view.toolbar_focus_targets(),
            "streams" => self.imp().streams_view.toolbar_focus_targets(),
            _ => return false,
        };
        let mapped: Vec<_> = controls
            .into_iter()
            .filter(|w| w.is_mapped() && w.is_sensitive() && w.is_visible())
            .filter_map(|w| ToolbarRect::of(&w, page).map(|rect| (w, rect)))
            .collect();
        let Some(current) = mapped.iter().position(|(w, _)| Self::is_within(focused, w)) else {
            return false; // Never intercept list rows or nested dialogs.
        };

        if matches!(key, gtk::gdk::Key::Left | gtk::gdk::Key::Right) {
            if let Some(entry) = mapped[current].0.downcast_ref::<gtk::SearchEntry>() {
                if entry.selection_bounds().is_some() {
                    return false;
                }
                let position = entry.position();
                let last = entry.text().chars().count() as i32;
                if (key == gtk::gdk::Key::Left && position > 0)
                    || (key == gtk::gdk::Key::Right && position < last)
                {
                    return false;
                }
            }
        }

        let geometry: Vec<_> = mapped.iter().map(|(_, rect)| *rect).collect();
        if let Some(index) = spatial_neighbor(&geometry, current, key) {
            return mapped[index].0.grab_focus();
        }
        // Upper and lower boundaries deliberately remain navigable without
        // introducing diagonal/historical Up-Down toolbar chains.
        if key == gtk::gdk::Key::Up {
            return self.focus_page_switcher();
        }
        if key == gtk::gdk::Key::Down {
            return match page_name {
                "live" => Self::find_descendant(page, &|w| w.is::<gtk::ListBox>())
                    .and_then(|list| list.downcast::<gtk::ListBox>().ok())
                    .and_then(|list| list.row_at_index(0))
                    .is_some_and(|row| row.grab_focus()),
                "mediathek" => self.imp().mediathek_view.focus_first_result(),
                "streams" => self.imp().streams_view.focus_first_result(),
                _ => false,
            };
        }
        false
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
            if self.focus_spatial_toolbar_key(
                self.imp()
                    .stack
                    .visible_child_name()
                    .as_deref()
                    .unwrap_or_default(),
                page,
                &focused,
                key,
            ) {
                return true;
            }
            if self.imp().stack.visible_child_name().as_deref() == Some("streams") {
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

            // Prototype remote route: Search -> Keyboard -> Results, and back.
            let list = Self::find_descendant(page, &|widget| widget.is::<gtk::ListBox>())
                .and_then(|widget| widget.downcast::<gtk::ListBox>().ok());

            if key == gtk::gdk::Key::Up {
                if let Some(first) = list.as_ref().and_then(|list| list.row_at_index(0)) {
                    let first_widget = first.upcast_ref::<gtk::Widget>();
                    if Self::is_within(&focused, first_widget) {
                        if focused != *first_widget {
                            return first.grab_focus();
                        }
                        return self.imp().live_view.focus_keyboard_prototype()
                            || search.as_ref().is_some_and(|entry| entry.grab_focus());
                    }
                }
                if self.imp().live_view.keyboard_prototype_has_focus(&focused) {
                    return search.as_ref().is_some_and(|entry| entry.grab_focus());
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
                    return self.imp().live_view.focus_keyboard_prototype()
                        || list
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
                && modifiers.is_empty()
                && matches!(
                    key,
                    gtk::gdk::Key::Back
                        | gtk::gdk::Key::Escape
                        | gtk::gdk::Key::Home
                        | gtk::gdk::Key::HomePage
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

        if !modifiers.is_empty() {
            return glib::Propagation::Proceed;
        }

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
    if !modifiers.is_empty() {
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
            for _ in 0..20 {
                while glib::MainContext::default().pending() {
                    glib::MainContext::default().iteration(false);
                }
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
        spin();
        let live = window.imp().live_view.toolbar_focus_targets();
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

        stack.set_visible_child_name("mediathek");
        spin();
        let med = window.imp().mediathek_view.toolbar_focus_targets();
        assert_eq!(med.len(), 6, "search, two toggles, three filters");
        assert!(med[0].grab_focus());
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&med[1]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&med[2]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Left));
        assert_focused(&med[1]);
        assert!(med[3].grab_focus());
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&med[4]);
        assert!(window.focus_boundary_key(gtk::gdk::Key::Right));
        assert_focused(&med[5]);

        stack.set_visible_child_name("streams");
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
