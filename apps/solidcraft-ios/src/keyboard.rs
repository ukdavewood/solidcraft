//! The on-screen keyboard: a number layout for number fields, and how much of the screen it
//! covers.
//!
//! winit's view takes the keyboard but ignores the layout and hides its height, and UIKit has no
//! safe Rust API, so (like `picker`) this module allows `unsafe`: it adds the `keyboardType`
//! trait method to winit's view class and listens for the keyboard's frame notifications.
#![allow(unsafe_code)]
use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicIsize, AtomicU32, AtomicUsize, Ordering};
use std::sync::OnceLock;

use objc2::ffi::{class_addMethod, class_getInstanceMethod, method_setImplementation};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Imp, Sel};
use objc2::{MainThreadMarker, msg_send, sel};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol, NSRect, NSString, NSValue};
use objc2_ui_kit::{UIApplication, UIKeyboardFrameEndUserInfoKey, UIScreen};

/// `UIKeyboardTypeNumbersAndPunctuation`: digits and symbols (expressions such as `20 mm` or
/// `2*3` still type), with a key back to the letters.
const NUMBERS_AND_PUNCTUATION: isize = 2;
const DEFAULT: isize = 0;

/// Return-key presses not yet handed to the UI.
static RETURNS: AtomicUsize = AtomicUsize::new(0);
/// The view's own `insertText:`, which the Return catcher passes every other character to.
static INSERT_TEXT: OnceLock<Imp> = OnceLock::new();
static CTX: OnceLock<egui::Context> = OnceLock::new();

static KEYBOARD_TYPE: AtomicIsize = AtomicIsize::new(DEFAULT);
/// The height (points, `f32` bits) of a docked keyboard over the screen's bottom; 0 when it is
/// hidden, floating or split.
static COVERED: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static OBSERVERS: RefCell<Vec<Retained<objc2::runtime::ProtocolObject<dyn NSObjectProtocol>>>> = const { RefCell::new(Vec::new()) };
    static INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `-insertText:` for winit's view. The software keyboard's Return arrives as the text "\n", which
/// winit passes on as a character and egui ignores: it becomes an Enter key press here instead.
extern "C-unwind" fn insert_text(this: *mut AnyObject, cmd: Sel, text: *mut AnyObject) {
    // SAFETY: UIKit passes an NSString (or nil, which `as_ref` turns into None).
    let is_return = unsafe { (text as *const NSString).as_ref() }.is_some_and(|t| t.to_string() == "\n");
    if is_return {
        RETURNS.fetch_add(1, Ordering::Relaxed);
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
    } else if let Some(original) = INSERT_TEXT.get() {
        // SAFETY: the original is `-insertText:`, which has exactly this signature.
        let f: extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) = unsafe { std::mem::transmute(*original) };
        f(this, cmd, text);
    }
}

/// Return presses since the last call.
pub fn take_returns() -> usize {
    RETURNS.swap(0, Ordering::Relaxed)
}

extern "C-unwind" fn keyboard_type(_this: *mut AnyObject, _cmd: Sel) -> isize {
    KEYBOARD_TYPE.load(Ordering::Relaxed)
}

/// The root view (winit's view), when there is a window.
fn view(mtm: MainThreadMarker) -> Option<Retained<objc2_ui_kit::UIView>> {
    #[allow(deprecated)]
    UIApplication::sharedApplication(mtm).keyWindow()?.rootViewController()?.view()
}

/// Add the trait method to winit's view class and start listening to the keyboard's frame.
/// Safe to call every frame; does the work once.
pub fn install(ctx: &egui::Context) {
    if INSTALLED.with(std::cell::Cell::get) {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(view) = view(mtm) else { return };
    INSTALLED.with(|i| i.set(true));
    let _ = CTX.set(ctx.clone());
    let class: *const objc2::runtime::AnyClass = view.class();
    // SAFETY: adds `-keyboardType` (returns an NSInteger: "q@:") to a class that lacks it; the
    // function has exactly that signature.
    unsafe {
        let imp: Imp = std::mem::transmute::<extern "C-unwind" fn(*mut AnyObject, Sel) -> isize, Imp>(keyboard_type);
        class_addMethod(class.cast_mut(), sel!(keyboardType), imp, c"q@:".as_ptr());
    }
    // SAFETY: replaces `-insertText:` on winit's view class with `insert_text`, keeping the original.
    unsafe {
        let method = class_getInstanceMethod(class, sel!(insertText:));
        if !method.is_null() {
            let new: Imp = std::mem::transmute::<extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject), Imp>(insert_text);
            if let Some(original) = method_setImplementation(method, new) {
                let _ = INSERT_TEXT.set(original);
            }
        }
    }
    let center = NSNotificationCenter::defaultCenter();
    for name in ["UIKeyboardWillChangeFrameNotification", "UIKeyboardWillHideNotification"] {
        let block = block2::RcBlock::new(move |note: NonNull<NSNotification>| {
            // SAFETY: the notification outlives the block call.
            let note = unsafe { note.as_ref() };
            let covered = note.userInfo().and_then(|info| {
                // SAFETY: the key is UIKit's constant for the end frame (an NSValue holding a CGRect).
                let value = info.objectForKey(unsafe { UIKeyboardFrameEndUserInfoKey })?;
                let value: &NSValue = value.downcast_ref()?;
                let frame: NSRect = unsafe { msg_send![value, CGRectValue] };
                #[allow(deprecated)]
                let screen = MainThreadMarker::new().map(|m| UIScreen::mainScreen(m).bounds())?;
                // Docked: as wide as the screen and at its bottom. A floating or split keyboard
                // leaves the layout alone.
                let docked = frame.size.width >= screen.size.width * 0.9 && frame.origin.y + frame.size.height >= screen.size.height - 1.0;
                docked.then_some((screen.size.height - frame.origin.y).max(0.0) as f32)
            });
            let hidden = note.name().to_string() == "UIKeyboardWillHideNotification";
            let height = if hidden { 0.0 } else { covered.unwrap_or(0.0) };
            COVERED.store(height.to_bits(), Ordering::Relaxed);
        });
        // SAFETY: the block is valid for the observer's life, which is the app's.
        let observer = unsafe { center.addObserverForName_object_queue_usingBlock(Some(&NSString::from_str(name)), None, None, &block) };
        OBSERVERS.with(|o| o.borrow_mut().push(observer));
    }
}

/// Offer the number layout (or the default) the next time the keyboard shows, and now if it is up.
pub fn set_numeric(numeric: bool) {
    let want = if numeric { NUMBERS_AND_PUNCTUATION } else { DEFAULT };
    if KEYBOARD_TYPE.swap(want, Ordering::Relaxed) == want {
        return;
    }
    if let Some(view) = MainThreadMarker::new().and_then(view) {
        view.reloadInputViews();
    }
}

/// Points of the screen's bottom the keyboard covers.
pub fn covered() -> f32 {
    f32::from_bits(COVERED.load(Ordering::Relaxed))
}
