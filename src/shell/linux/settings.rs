//! Settings: a libadwaita preferences dialog with one combo row per
//! continuous-mode setting and the resulting throughput data per hour.
//! Nothing is stored; the values live in the core for this launch.

use std::rc::Rc;

use adw::prelude::*;

use super::app;
use super::tip::Tipped;
use crate::view::frame::{self, Setting, Settings};

pub struct SettingsUi {
    pub dialog: adw::PreferencesDialog,
    rows: Vec<(Setting, adw::ComboRow)>,
    per_hour: gtk::Label,
    per_hour_tip: Rc<Tipped>,
}

impl SettingsUi {
    pub fn new(s: &Settings) -> SettingsUi {
        let group = adw::PreferencesGroup::new();
        let rows = s
            .rows
            .iter()
            .map(|r| {
                let options: Vec<&str> = r.options.iter().map(String::as_str).collect();
                let row = adw::ComboRow::builder()
                    .title(r.label)
                    .model(&gtk::StringList::new(&options))
                    .selected(r.selected as u32)
                    .build();
                let id = r.id;
                row.connect_selected_notify(move |row| {
                    app().set_setting(id, row.selected() as usize);
                });
                group.add(&row);
                (id, row)
            })
            .collect();
        let per_hour = gtk::Label::builder()
            .css_classes(["numeric", "dim-label"])
            .build();
        let row = adw::ActionRow::builder()
            .title(s.data_per_hour.label.as_str())
            .build();
        row.add_suffix(&per_hour);
        let per_hour_tip = Tipped::attach(&row);
        group.add(&row);
        let page = adw::PreferencesPage::new();
        page.add(&group);
        let dialog = adw::PreferencesDialog::builder()
            .title("Settings")
            .search_enabled(false)
            .build();
        dialog.add(&page);
        let ui = SettingsUi {
            dialog,
            rows,
            per_hour,
            per_hour_tip,
        };
        ui.update(s);
        ui
    }

    /// Shows `s`. Called while the app ignores its own widget signals.
    pub fn update(&self, s: &Settings) {
        for ((id, row), r) in self.rows.iter().zip(&s.rows) {
            debug_assert_eq!(*id, r.id);
            if row.selected() as usize != r.selected {
                row.set_selected(r.selected as u32);
            }
        }
        let kv = &s.data_per_hour;
        self.per_hour
            .set_text(kv.value.as_deref().unwrap_or(frame::MISSING));
        self.per_hour_tip.set(&kv.tip);
    }
}
