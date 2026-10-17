// SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org>
// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use adw::{gio, glib, gtk, prelude::*, subclass::prelude::*};
use indexmap::IndexMap;
use smart_default::SmartDefault;

use crate::{
    application::TvApplication,
    settings::TvSettings,
    zapp::{ChannelId, ChannelInfo},
};

use super::selector_row::TvLiveChannelSelectorRow;

mod imp {
    use crate::utils::{show_error, spawn, ListStoreExtManual};

    use super::*;

    #[derive(Debug, SmartDefault, gtk::CompositeTemplate)]
    #[template(file = "src/preferences/live/selector.blp")]
    pub struct TvLiveChannelSelector {
        #[template_child]
        pub(super) visible_channel_rows: TemplateChild<gtk::ListBox>,
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
                    show_error(&gettextrs::gettext("Failed to load livestream channel settings"));
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

            self.visible_channels.remove(position);
            self.visible_channels.insert(new_position, row);
            // the row lost the focus when it was removed
            row.grab_focus();
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

            self.visible_channel_rows
                .bind_model(Some(&self.visible_channels), |row| {
                    row.clone().downcast().unwrap()
                });
            self.hidden_channel_rows
                .bind_model(Some(&self.hidden_channels), |row| {
                    row.clone().downcast().unwrap()
                });

            self.loading.set(true);
            spawn(glib::clone!(
                #[weak(rename_to = slf)]
                self,
                async move { slf.load().await }
            ));

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
