// SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org>
// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    sync::OnceLock,
};

use adw::{gdk, glib, glib::subclass::Signal, gtk, prelude::*, subclass::prelude::*};

use crate::{channel_icons::load_channel_icon, zapp::ChannelId};

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate, glib::Properties)]
    #[template(file = "src/preferences/live/selector_row.blp")]
    #[properties(wrapper_type = super::TvLiveChannelSelectorRow)]
    pub struct TvLiveChannelSelectorRow {
        #[template_child]
        icon: TemplateChild<gtk::Image>,
        #[template_child]
        fallback_icon: TemplateChild<gtk::Label>,
        #[template_child]
        switch: TemplateChild<gtk::Switch>,
        #[template_child]
        move_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        drag_source: TemplateChild<gtk::DragSource>,
        #[template_child]
        drop_target: TemplateChild<gtk::DropTarget>,

        #[property(get, construct_only)]
        channel_id: RefCell<ChannelId>,
        #[property(get, construct_only)]
        channel_name: RefCell<String>,
        #[property(get, set)]
        visible: Cell<bool>,

        drag_x: Cell<f64>,
        drag_y: Cell<f64>,
    }

    #[gtk::template_callbacks]
    impl TvLiveChannelSelectorRow {
        #[template_callback]
        fn drag_prepare(&self, x: f64, y: f64) -> Option<gdk::ContentProvider> {
            self.drag_x.set(x);
            self.drag_y.set(y);
            Some(gdk::ContentProvider::for_value(&self.obj().to_value()))
        }
        #[template_callback]
        fn drag_begin(&self, drag: gdk::Drag) {
            let drag_widget = gtk::ListBox::builder()
                .width_request(self.obj().width())
                .height_request(self.obj().height())
                .build();

            let drag_row: super::TvLiveChannelSelectorRow = glib::Object::builder()
                .property("channel-id", &*self.channel_id.borrow())
                .property("channel-name", &*self.channel_name.borrow())
                .property("visible", true)
                .build();
            drag_widget.append(&drag_row);
            drag_widget.drag_highlight_row(&drag_row);

            let drag_icon: gtk::DragIcon = gtk::DragIcon::for_drag(&drag).downcast().unwrap();
            drag_icon.set_child(Some(&drag_widget));
            drag.set_hotspot(self.drag_x.get() as i32, self.drag_y.get() as i32)
        }
        #[template_callback]
        fn drop(&self, value: glib::BoxedValue, #[rest] _: &[glib::Value]) -> bool {
            match value.get::<super::TvLiveChannelSelectorRow>() {
                Ok(row) => {
                    self.obj().emit_received_drop(&row);
                    true
                }
                _ => false,
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TvLiveChannelSelectorRow {
        const NAME: &'static str = "TvLiveChannelSelectorRow";
        type Type = super::TvLiveChannelSelectorRow;
        type ParentType = adw::ActionRow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_callbacks();

            klass.install_action("row.move-up", None, |row, _, _| row.emit_move_requested(-1));
            klass.install_action("row.move-down", None, |row, _, _| {
                row.emit_move_requested(1)
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for TvLiveChannelSelectorRow {
        fn constructed(&self) {
            self.parent_constructed();

            load_channel_icon(
                Some(self.channel_id.borrow().as_ref()),
                &self.icon,
                &self.fallback_icon,
                16,
            );

            super::selector::install_channel_menu_remote_handler(&self.move_button);
        }
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<[Signal; 2]> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                [
                    Signal::builder("received-drop")
                        .param_types([super::TvLiveChannelSelectorRow::static_type()])
                        .build(),
                    // the row should move by the given number of positions
                    Signal::builder("move-requested")
                        .param_types([i32::static_type()])
                        .build(),
                ]
            })
        }
    }
    impl WidgetImpl for TvLiveChannelSelectorRow {}
    impl ListBoxRowImpl for TvLiveChannelSelectorRow {}
    impl PreferencesRowImpl for TvLiveChannelSelectorRow {}
    impl ActionRowImpl for TvLiveChannelSelectorRow {}
}

glib::wrapper! {
    pub struct TvLiveChannelSelectorRow(ObjectSubclass<imp::TvLiveChannelSelectorRow>)
        @extends gtk::Widget, gtk::ListBoxRow, adw::PreferencesRow, adw::ActionRow,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Actionable;
}

impl TvLiveChannelSelectorRow {
    pub fn new(id: &ChannelId, name: &str) -> Self {
        glib::Object::builder()
            .property("channel-id", id)
            .property("channel-name", name)
            .build()
    }
    pub fn emit_received_drop(&self, row: &TvLiveChannelSelectorRow) {
        self.emit_by_name::<()>("received-drop", &[&row]);
    }
    fn emit_move_requested(&self, offset: i32) {
        self.emit_by_name::<()>("move-requested", &[&offset]);
    }
    pub fn connect_move_requested(&self, f: impl Fn(&Self, i32) + 'static) {
        self.connect_local("move-requested", false, move |args| {
            f(&args[0].get().unwrap(), args[1].get().unwrap());
            None
        });
    }
    pub fn connect_received_drop(&self, f: impl Fn(&Self, &TvLiveChannelSelectorRow) + 'static) {
        self.connect_local("received-drop", false, move |args| {
            f(&args[0].get().unwrap(), &args[1].get().unwrap());
            None
        });
    }
}
