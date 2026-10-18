// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Virtual indexed GTK list model. A 10k result allocates 10k *empty pointers*,
//! not 10k widgets or hydrated channels. GTK requests slot objects on demand.
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    sync::OnceLock,
};

use adw::{gio, glib, prelude::*, subclass::prelude::*};

use super::model::ChannelEffective;

pub(super) const PAGE_SIZE: u32 = 128;
const RETAINED_PAGES: usize = 5;

mod slot_imp {
    use super::*;

    #[derive(Default)]
    pub struct StreamSlot {
        pub position: Cell<u32>,
        pub channel: RefCell<Option<ChannelEffective>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StreamSlot {
        const NAME: &'static str = "BroadcasterStreamSlot";
        type Type = super::StreamSlot;
    }

    impl ObjectImpl for StreamSlot {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES
                .get_or_init(|| vec![glib::ParamSpecBoolean::builder("ready").read_only().build()])
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "ready" => self.channel.borrow().is_some().to_value(),
                _ => unreachable!("unknown StreamSlot property"),
            }
        }
    }
}

glib::wrapper! {
    pub struct StreamSlot(ObjectSubclass<slot_imp::StreamSlot>);
}

impl StreamSlot {
    fn new(position: u32, channel: Option<ChannelEffective>) -> Self {
        let slot: Self = glib::Object::new();
        slot.imp().position.set(position);
        *slot.imp().channel.borrow_mut() = channel;
        slot
    }

    pub(super) fn position(&self) -> u32 {
        self.imp().position.get()
    }

    pub(super) fn channel(&self) -> Option<ChannelEffective> {
        self.imp().channel.borrow().clone()
    }

    fn update(&self, channel: Option<ChannelEffective>) {
        *self.imp().channel.borrow_mut() = channel;
        self.notify("ready");
    }
}

mod rows_imp {
    use super::*;

    #[derive(Default)]
    pub struct StreamRows {
        pub(super) slots: RefCell<Vec<Option<StreamSlot>>>,
        pub pages: RefCell<BTreeMap<u32, Vec<ChannelEffective>>>,
        pub recency: RefCell<VecDeque<u32>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StreamRows {
        const NAME: &'static str = "BroadcasterStreamRows";
        type Type = super::StreamRows;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for StreamRows {}

    impl gio::subclass::prelude::ListModelImpl for StreamRows {
        fn item_type(&self) -> glib::Type {
            StreamSlot::static_type()
        }

        fn n_items(&self) -> u32 {
            self.slots.borrow().len() as u32
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            let mut slots = self.slots.borrow_mut();
            let item = slots.get_mut(position as usize)?;
            if item.is_none() {
                let page = position / PAGE_SIZE;
                let data = self
                    .pages
                    .borrow()
                    .get(&page)
                    .and_then(|items| items.get((position % PAGE_SIZE) as usize))
                    .cloned();
                *item = Some(StreamSlot::new(position, data));
            }
            item.clone().map(|slot| slot.upcast())
        }
    }
}

glib::wrapper! {
    pub struct StreamRows(ObjectSubclass<rows_imp::StreamRows>)
        @implements gio::ListModel;
}

impl StreamRows {
    pub(super) fn new() -> Self {
        glib::Object::new()
    }

    pub(super) fn reset(&self, total: u32) {
        let imp = self.imp();
        let old = imp.slots.borrow().len() as u32;
        imp.pages.borrow_mut().clear();
        imp.recency.borrow_mut().clear();
        *imp.slots.borrow_mut() = vec![None; total as usize];
        self.items_changed(0, old, total);
    }

    pub(super) fn has_page(&self, page: u32) -> bool {
        self.imp().pages.borrow().contains_key(&page)
    }

    pub(super) fn channel(&self, position: u32) -> Option<ChannelEffective> {
        let imp = self.imp();
        let page = position / PAGE_SIZE;
        imp.pages
            .borrow()
            .get(&page)
            .and_then(|items| items.get((position % PAGE_SIZE) as usize))
            .cloned()
    }

    /// Notify only live row slots; the total and scroll position stay stable.
    pub(super) fn insert_page(&self, page: u32, channels: Vec<ChannelEffective>) {
        let imp = self.imp();
        let start = page.saturating_mul(PAGE_SIZE);
        if start >= imp.slots.borrow().len() as u32 {
            return;
        }
        {
            let slots = imp.slots.borrow();
            for (offset, channel) in channels.iter().enumerate() {
                if let Some(Some(slot)) = slots.get(start as usize + offset) {
                    slot.update(Some(channel.clone()));
                }
            }
        }
        imp.pages.borrow_mut().insert(page, channels);
        let mut recency = imp.recency.borrow_mut();
        recency.retain(|other| *other != page);
        recency.push_back(page);
        while recency.len() > RETAINED_PAGES {
            if let Some(evicted) = recency.pop_front() {
                imp.pages.borrow_mut().remove(&evicted);
                let slots = imp.slots.borrow();
                let begin = evicted as usize * PAGE_SIZE as usize;
                let end = (begin + PAGE_SIZE as usize).min(slots.len());
                for slot in slots[begin..end].iter().flatten() {
                    slot.update(None);
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn created_slots(&self) -> usize {
        self.imp()
            .slots
            .borrow()
            .iter()
            .filter(|slot| slot.is_some())
            .count()
    }

    #[cfg(test)]
    pub(super) fn cached_pages(&self) -> usize {
        self.imp().pages.borrow().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_thousand_positions_are_lazy() {
        let rows = StreamRows::new();
        rows.reset(10_000);
        assert_eq!(rows.n_items(), 10_000);
        assert_eq!(rows.created_slots(), 0);
        let slot = rows.item(9_999).unwrap().downcast::<StreamSlot>().unwrap();
        assert_eq!(slot.position(), 9_999);
        assert!(slot.channel().is_none());
        assert_eq!(rows.created_slots(), 1);
        assert_eq!(rows.cached_pages(), 0);
    }
}
