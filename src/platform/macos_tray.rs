//! The optional menu bar item: an `NSStatusItem` showing live values, with
//! a menu to start, pause and stop runs. Main thread only.

use std::cell::RefCell;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSCellImagePosition, NSColor,
    NSControlStateValueOff, NSControlStateValueOn, NSFont, NSFontAttributeName,
    NSFontWeightRegular, NSForegroundColorAttributeName, NSImage, NSMenu, NSMenuItem,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSStatusBar, NSStatusItem,
    NSTextAlignment, NSTextTab, NSVariableStatusItemLength,
};
use objc2_foundation::{NSArray, NSAttributedString, NSDictionary, NSObject, NSString};

use super::{TrayAction, TrayStatus, Wake};

/// Where the value column of the menu's rows starts, in points.
const VALUE_TAB: f64 = 210.0;

static ACTIONS: Mutex<Vec<TrayAction>> = Mutex::new(Vec::new());
static WAKE: Mutex<Option<Wake>> = Mutex::new(None);

fn push(action: TrayAction) {
    ACTIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(action);
    let wake = WAKE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(wake) = wake {
        wake();
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and `Target`
    // implements no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RawSpeedTestMenuTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(runOnce:))]
        fn run_once(&self, _sender: Option<&AnyObject>) {
            push(TrayAction::RunOnce);
        }

        #[unsafe(method(runContinuously:))]
        fn run_continuously(&self, _sender: Option<&AnyObject>) {
            push(TrayAction::RunContinuously);
        }

        #[unsafe(method(pauseResume:))]
        fn pause_resume(&self, _sender: Option<&AnyObject>) {
            push(TrayAction::PauseResume);
        }

        #[unsafe(method(stop:))]
        fn stop(&self, _sender: Option<&AnyObject>) {
            push(TrayAction::Stop);
        }

        #[unsafe(method(showWindow:))]
        fn show_window(&self, _sender: Option<&AnyObject>) {
            push(TrayAction::ShowWindow);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            push(TrayAction::Quit);
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Target> {
        let this = Target::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

struct Tray {
    item: Retained<NSStatusItem>,
    // Kept alive for the status item's lifetime.
    _menu: Retained<NSMenu>,
    _target: Retained<Target>,
    rows: Vec<Retained<NSMenuItem>>,
    status: Retained<NSMenuItem>,
    run_once: Retained<NSMenuItem>,
    run_continuously: Retained<NSMenuItem>,
    pause: Retained<NSMenuItem>,
    stop: Retained<NSMenuItem>,
    shown: Option<TrayStatus>,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

fn menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<Sel>,
    target: &Target,
) -> Retained<NSMenuItem> {
    // SAFETY: plain initializer; the target outlives the item (both held by
    // `Tray`), and every action selector is implemented by `Target`.
    unsafe {
        let item = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(""),
        );
        if action.is_some() {
            let target: &AnyObject = target.as_ref();
            item.setTarget(Some(target));
        }
        item
    }
}

/// `label<TAB>value` with the value right-aligned at a tab stop, in the menu
/// font with tabular digits.
fn row_title(label: &str, value: &str, color: &NSColor) -> Retained<NSAttributedString> {
    // SAFETY: AppKit attribute keys are immutable statics; values have the
    // types those keys expect.
    unsafe {
        let size = NSFont::menuFontOfSize(0.0).pointSize();
        let font = NSFont::monospacedDigitSystemFontOfSize_weight(size, NSFontWeightRegular);
        let para = NSMutableParagraphStyle::new();
        let tab = NSTextTab::initWithTextAlignment_location_options(
            NSTextTab::alloc(),
            NSTextAlignment::Right,
            VALUE_TAB,
            &NSDictionary::new(),
        );
        para.setTabStops(Some(&NSArray::from_retained_slice(&[tab])));
        let text = if value.is_empty() {
            label.to_owned()
        } else {
            format!("{label}\t{value}")
        };
        let keys: [&NSString; 3] = [
            NSFontAttributeName,
            NSParagraphStyleAttributeName,
            NSForegroundColorAttributeName,
        ];
        let values: [&AnyObject; 3] = [font.as_ref(), para.as_ref(), color.as_ref()];
        let attrs = NSDictionary::from_slices(&keys, &values);
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(&text),
            Some(&attrs),
        )
    }
}

impl Tray {
    fn new(mtm: MainThreadMarker) -> Tray {
        let target = Target::new(mtm);
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(mtm) {
            // A system symbol, so nothing is embedded; drawn as a template
            // image so it follows the menu bar's appearance.
            let image = ["gauge.with.dots.needle.33percent", "speedometer"]
                .iter()
                .find_map(|name| {
                    NSImage::imageWithSystemSymbolName_accessibilityDescription(
                        &NSString::from_str(name),
                        Some(&NSString::from_str("Raw Speed Test")),
                    )
                });
            if let Some(image) = image {
                image.setTemplate(true);
                button.setImage(Some(&image));
                button.setImagePosition(NSCellImagePosition::ImageLeading);
            } else {
                button.setTitle(&NSString::from_str("Raw Speed Test"));
            }
        }

        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
        menu.setAutoenablesItems(false);
        // Value rows have no action but stay enabled: disabled items are
        // drawn dimmed, below the contrast the values need.
        let rows: Vec<_> = (0..5).map(|_| menu_item(mtm, "", None, &target)).collect();
        for r in &rows {
            menu.addItem(r);
        }
        let status = menu_item(mtm, "", None, &target);
        menu.addItem(&status);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let run_once = menu_item(mtm, "Run once", Some(sel!(runOnce:)), &target);
        let run_continuously = menu_item(
            mtm,
            "Run continuously",
            Some(sel!(runContinuously:)),
            &target,
        );
        let pause = menu_item(mtm, "Pause", Some(sel!(pauseResume:)), &target);
        let stop = menu_item(mtm, "Stop", Some(sel!(stop:)), &target);
        for i in [&run_once, &run_continuously, &pause, &stop] {
            menu.addItem(i);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&menu_item(
            mtm,
            "Show window",
            Some(sel!(showWindow:)),
            &target,
        ));
        menu.addItem(&menu_item(
            mtm,
            "Quit Raw Speed Test",
            Some(sel!(quit:)),
            &target,
        ));
        item.setMenu(Some(&menu));
        Tray {
            item,
            _menu: menu,
            _target: target,
            rows,
            status,
            run_once,
            run_continuously,
            pause,
            stop,
            shown: None,
        }
    }

    fn apply(&mut self, s: &TrayStatus, mtm: MainThreadMarker) {
        if self.shown.as_ref() == Some(s) {
            return;
        }
        if let Some(button) = self.item.button(mtm) {
            // SAFETY: font and attribute key statics, as in `row_title`.
            let title = unsafe {
                let size = NSFont::menuBarFontOfSize(0.0).pointSize();
                let font =
                    NSFont::monospacedDigitSystemFontOfSize_weight(size, NSFontWeightRegular);
                let keys: [&NSString; 1] = [NSFontAttributeName];
                let values: [&AnyObject; 1] = [font.as_ref()];
                let attrs = NSDictionary::from_slices(&keys, &values);
                let text = if s.title.is_empty() {
                    String::new()
                } else {
                    format!(" {}", s.title)
                };
                NSAttributedString::initWithString_attributes(
                    NSAttributedString::alloc(),
                    &NSString::from_str(&text),
                    Some(&attrs),
                )
            };
            button.setAttributedTitle(&title);
        }
        let label = NSColor::labelColor();
        for (item, (k, v)) in self.rows.iter().zip(&s.rows) {
            item.setAttributedTitle(Some(&row_title(k, v, &label)));
        }
        self.status.setAttributedTitle(Some(&row_title(
            &s.status,
            "",
            &NSColor::secondaryLabelColor(),
        )));
        let state = |on: bool| {
            if on {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            }
        };
        self.run_once.setState(state(s.one_shot_running));
        self.run_continuously.setState(state(s.continuous_running));
        let active = s.one_shot_running || s.continuous_running;
        self.pause.setTitle(&NSString::from_str(if s.paused {
            "Resume"
        } else {
            "Pause"
        }));
        self.pause.setEnabled(active);
        self.stop.setEnabled(active);
        self.shown = Some(s.clone());
    }
}

pub fn set(on: bool, wake: Wake) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    *WAKE.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake);
    TRAY.with(|t| {
        let mut t = t.borrow_mut();
        match (on, t.is_some()) {
            (true, false) => *t = Some(Tray::new(mtm)),
            (false, true) => {
                if let Some(tray) = t.take() {
                    NSStatusBar::systemStatusBar().removeStatusItem(&tray.item);
                }
            }
            _ => {}
        }
    });
}

pub fn update(status: &TrayStatus) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    TRAY.with(|t| {
        if let Ok(mut t) = t.try_borrow_mut()
            && let Some(tray) = t.as_mut()
        {
            tray.apply(status, mtm);
        }
    });
}

pub fn take_actions() -> Vec<TrayAction> {
    std::mem::take(&mut *ACTIONS.lock().unwrap_or_else(|e| e.into_inner()))
}

pub fn set_dock_icon(visible: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let policy = if visible {
        NSApplicationActivationPolicy::Regular
    } else {
        NSApplicationActivationPolicy::Accessory
    };
    NSApplication::sharedApplication(mtm).setActivationPolicy(policy);
}

pub fn activate() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    if app.respondsToSelector(sel!(activate)) {
        app.activate();
    } else {
        // `activate` arrived in macOS 14; older systems only have this.
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}

pub fn quit() {
    let Some(mtm) = MainThreadMarker::new() else {
        std::process::exit(0);
    };
    NSApplication::sharedApplication(mtm).terminate(None);
}
