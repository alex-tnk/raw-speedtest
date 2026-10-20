//! The request, connection and error logs: one column view per tab over a
//! lazy list model that reads pages of rows from the session only for the
//! rows GTK asks for. Rows are newest first, so new records are inserted
//! at the top and records the session's ring drops leave from the bottom.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::SystemTime;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib, pango};

use super::style;
use super::tip;
use crate::app::session::Session;
use crate::view::log::{self, Row, Tab};

/// Rows read from the session at a time.
const PAGE: u64 = 128;
/// Pages kept; GTK asks only for rows near the visible ones.
const KEEP_PAGES: usize = 16;
/// Height of the log table.
pub const HEIGHT: i32 = 280;

/// What identifies the rows of a tab: the run, rows held, and rows ever
/// added (held + dropped by the ring).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Counts {
    pub run: Option<SystemTime>,
    pub len: u64,
    pub total: u64,
}

pub fn counts(s: &Session, tab: Tab) -> Counts {
    let (len, evicted) = match tab {
        Tab::Requests => (s.requests.len(), s.requests.evicted()),
        Tab::Connections => (s.conns.len(), s.conns.evicted()),
        Tab::Errors => (s.errors.len(), s.errors.evicted()),
    };
    Counts {
        run: s.wall_start,
        len: len as u64,
        total: len as u64 + evicted,
    }
}

/// Reads the rows of run `run` with absolute indexes `lo..=hi` (0 = the
/// run's first row), newest first.
pub type Fetch = Box<dyn Fn(Option<SystemTime>, u64, u64) -> Vec<Row>>;

/// Rows `lo..=hi` of `tab` in run `run`, newest first, translated to the
/// session's current positions; rows no longer held are left out.
pub fn fetch(s: &Session, tab: Tab, run: Option<SystemTime>, lo: u64, hi: u64) -> Vec<Row> {
    let c = counts(s, tab);
    if c.run != run || hi >= c.total {
        return Vec::new();
    }
    let first = c.total - 1 - hi;
    log::rows(s, tab, first as usize, (hi - lo + 1) as usize)
}

/// Row objects for the absolute indexes `lo..lo + objs.len()`.
struct Page {
    lo: u64,
    objs: Vec<glib::BoxedAnyObject>,
}

impl Page {
    fn get(&self, abs: u64) -> Option<glib::BoxedAnyObject> {
        let i = usize::try_from(abs.checked_sub(self.lo)?).ok()?;
        self.objs.get(i).cloned()
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct LogModel {
        pub(super) counts: Cell<Counts>,
        /// Pages of row objects by absolute page index.
        pub(super) pages: RefCell<HashMap<u64, Page>>,
        pub(super) fetch: RefCell<Option<Fetch>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LogModel {
        const NAME: &'static str = "RstLogModel";
        type Type = super::LogModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for LogModel {}

    impl ListModelImpl for LogModel {
        fn item_type(&self) -> glib::Type {
            glib::BoxedAnyObject::static_type()
        }

        fn n_items(&self) -> u32 {
            self.counts.get().len as u32
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            let c = self.counts.get();
            let p = u64::from(position);
            if p >= c.len {
                return None;
            }
            let abs = c.total - 1 - p;
            let page = abs / PAGE;
            let mut pages = self.pages.borrow_mut();
            if let Some(o) = pages.get(&page).and_then(|pg| pg.get(abs)) {
                return Some(o.upcast());
            }
            // The rows of this page the model holds, up to the newest.
            let lo = (page * PAGE).max(c.total - c.len);
            let hi = ((page + 1) * PAGE).min(c.total) - 1;
            let rows = self
                .fetch
                .borrow()
                .as_ref()
                .map(|f| f(c.run, lo, hi))
                .unwrap_or_default();
            if rows.len() as u64 != hi - lo + 1 {
                // The session moved on under the model (a new run, or the
                // ring dropped these rows); the next sync catches up.
                let empty = Row {
                    cells: Vec::new(),
                    tip: None,
                };
                return Some(glib::BoxedAnyObject::new(empty).upcast());
            }
            if pages.len() >= KEEP_PAGES {
                pages.clear();
            }
            // Fetched newest first; stored oldest first from `lo`.
            let objs: Vec<_> = rows
                .into_iter()
                .rev()
                .map(glib::BoxedAnyObject::new)
                .collect();
            let o = objs[(abs - lo) as usize].clone();
            pages.insert(page, Page { lo, objs });
            Some(o.upcast())
        }
    }
}

glib::wrapper! {
    pub struct LogModel(ObjectSubclass<imp::LogModel>) @implements gio::ListModel;
}

impl LogModel {
    pub fn new(fetch: Fetch) -> LogModel {
        let m: LogModel = glib::Object::new();
        *m.imp().fetch.borrow_mut() = Some(fetch);
        m
    }

    /// Follows the session: new rows are inserted at the top and rows the
    /// ring dropped are removed from the bottom; a new run replaces all.
    /// True if rows were added.
    pub fn sync(&self, now: Counts) -> bool {
        let imp = self.imp();
        let old = imp.counts.get();
        if now == old {
            return false;
        }
        let added = now.total.saturating_sub(old.total);
        let dropped = (now.total - now.len).saturating_sub(old.total - old.len);
        let fresh =
            now.run != old.run || now.total < old.total || dropped >= old.len || added > now.len;
        if fresh {
            imp.pages.borrow_mut().clear();
            imp.counts.set(now);
            self.items_changed(0, old.len as u32, now.len as u32);
            return now.len > 0;
        }
        // Absolute indexes are stable, so cached pages stay valid.
        if dropped > 0 {
            imp.counts.set(Counts {
                len: old.len - dropped,
                ..old
            });
            self.items_changed((old.len - dropped) as u32, dropped as u32, 0);
        }
        imp.counts.set(now);
        if added > 0 {
            self.items_changed(0, 0, added as u32);
        }
        added > 0
    }
}

/// One tab: its model and the table showing it.
pub struct LogTab {
    pub tab: Tab,
    pub model: LogModel,
    pub view: gtk::ColumnView,
    pub scroll: gtk::ScrolledWindow,
}

impl LogTab {
    pub fn new(tab: Tab, fetch: Fetch) -> LogTab {
        let model = LogModel::new(fetch);
        let view = gtk::ColumnView::builder()
            .model(&gtk::NoSelection::new(Some(model.clone())))
            .reorderable(false)
            .css_classes(["data-table"])
            .build();
        let cols = log::columns(tab);
        for (i, c) in cols.iter().enumerate() {
            let col = gtk::ColumnViewColumn::builder()
                .title(c.title)
                .factory(&cell_factory(i, c.numeric))
                .resizable(true)
                .fixed_width(c.width.round() as i32)
                .expand(i + 1 == cols.len())
                .build();
            view.append_column(&col);
        }
        let scroll = gtk::ScrolledWindow::builder()
            .child(&view)
            .min_content_height(HEIGHT)
            .max_content_height(HEIGHT)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .has_frame(true)
            .build();
        LogTab {
            tab,
            model,
            view,
            scroll,
        }
    }

    pub fn sync(&self, now: Counts) {
        // Stay on the newest row while it is in view; otherwise GTK keeps
        // the rows being read in place as new ones arrive above.
        let at_top = self.scroll.vadjustment().value() <= 0.5;
        if self.model.sync(now) && at_top {
            self.view
                .scroll_to(0, None, gtk::ListScrollFlags::NONE, None);
        }
    }
}

fn row_of(item: &gtk::ListItem) -> Option<glib::BoxedAnyObject> {
    item.item().and_downcast::<glib::BoxedAnyObject>()
}

fn cell_factory(i: usize, numeric: bool) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(move |_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let l = gtk::Label::builder()
            .xalign(if numeric { 1.0 } else { 0.0 })
            .ellipsize(pango::EllipsizeMode::End)
            .has_tooltip(true)
            .build();
        let weak = item.downgrade();
        l.connect_query_tooltip(move |_, _, _, _, tooltip| {
            let Some(row) = weak.upgrade().and_then(|it| row_of(&it)) else {
                return false;
            };
            let row = row.borrow::<Row>();
            let Some(t) = row.tip.as_ref().filter(|t| !t.is_empty()) else {
                return false;
            };
            tooltip.set_custom(Some(&tip::content(t)));
            true
        });
        item.set_child(Some(&l));
    });
    f.connect_bind(move |_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(l), Some(row)) = (item.child().and_downcast::<gtk::Label>(), row_of(item)) else {
            return;
        };
        let row = row.borrow::<Row>();
        let cell = row.cells.get(i);
        l.set_text(cell.map_or("", |c| c.text.as_str()));
        let mut classes: Vec<&str> = Vec::with_capacity(2);
        if numeric {
            classes.push("numeric");
        }
        if let Some(c) = cell {
            classes.extend(style::fg(c.color));
        }
        l.set_css_classes(&classes);
    });
    f
}

/// The tab bar and the three tables.
pub struct Logs {
    pub root: gtk::Box,
    stack: gtk::Stack,
    pub tabs: Vec<LogTab>,
    labels: RefCell<Vec<String>>,
}

impl Logs {
    pub fn new(
        fetch: impl Fn(Tab, Option<SystemTime>, u64, u64) -> Vec<Row> + 'static,
    ) -> Rc<Logs> {
        let fetch = Rc::new(fetch);
        let stack = gtk::Stack::new();
        let tabs: Vec<LogTab> = [Tab::Requests, Tab::Connections, Tab::Errors]
            .into_iter()
            .map(|tab| {
                let f = fetch.clone();
                let t = LogTab::new(tab, Box::new(move |run, lo, hi| f(tab, run, lo, hi)));
                stack.add_titled(&t.scroll, None, &tab.label(0));
                t
            })
            .collect();
        let switcher = gtk::StackSwitcher::builder()
            .stack(&stack)
            .halign(gtk::Align::Start)
            .build();
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(super::panels::S8)
            .build();
        root.append(&switcher);
        root.append(&stack);
        Rc::new(Logs {
            root,
            stack,
            labels: RefCell::new(tabs.iter().map(|t| t.tab.label(0)).collect()),
            tabs,
        })
    }

    /// Tab labels with their row counts.
    pub fn counts(&self, n: [usize; 3]) {
        let mut labels = self.labels.borrow_mut();
        for ((t, n), old) in self.tabs.iter().zip(n).zip(labels.iter_mut()) {
            let l = t.tab.label(n);
            if *old != l {
                self.stack.page(&t.scroll).set_title(&l);
                *old = l;
            }
        }
    }
}
