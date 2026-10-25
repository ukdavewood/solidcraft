//! The system document picker (the Files sheet: iCloud Drive, On My iPad, other apps' folders,
//! subfolders, AirDrop'd files) for Open.
//!
//! UIKit has no safe Rust API, so this module is the one place that allows `unsafe`: it calls
//! `UIDocumentPickerViewController` through the `objc2` bindings. The picker is asked for a
//! *copy* of the chosen file (inside the app's sandbox), so the rest of the app opens an ordinary
//! path and never deals with security-scoped URLs.
#![allow(unsafe_code)]
use std::cell::RefCell;
use std::rc::Rc;
use std::task::Poll;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSString, NSURL};
use objc2_ui_kit::{UIApplication, UIDocumentPickerDelegate, UIDocumentPickerViewController};
use objc2_uniform_type_identifiers::UTType;
use solidcraft_ui_egui::FilePickerFuture;

/// The picker's answer: `Some(None)` is a cancel.
type Answer = Rc<RefCell<Option<Option<String>>>>;

struct Ivars {
    answer: Answer,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SolidCraftPickerDelegate"]
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UIDocumentPickerDelegate for Delegate {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        fn did_pick(&self, _picker: &UIDocumentPickerViewController, urls: &NSArray<NSURL>) {
            let path = urls.firstObject().and_then(|u| u.path()).map(|p| p.to_string());
            *self.ivars().answer.borrow_mut() = Some(path);
        }

        #[unsafe(method(documentPickerWasCancelled:))]
        fn cancelled(&self, _picker: &UIDocumentPickerViewController) {
            *self.ivars().answer.borrow_mut() = Some(None);
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, answer: Answer) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { answer });
        // SAFETY: `init` on an allocated NSObject subclass whose ivars are set.
        unsafe { msg_send![super(this), init] }
    }
}

thread_local! {
    /// The picker holds its delegate weakly: kept here while a picker is up.
    static DELEGATE: RefCell<Option<Retained<Delegate>>> = const { RefCell::new(None) };
}

/// Show the picker for any file; `None` when it cannot be shown (not on the main thread, or no
/// window to present from), so the caller can use the in-app browser instead.
pub fn open() -> Option<FilePickerFuture> {
    let mtm = MainThreadMarker::new()?;
    let answer: Answer = Rc::default();
    let delegate = Delegate::new(mtm, answer.clone());
    let types: Vec<Retained<UTType>> = ["public.data", "public.item"].iter().filter_map(|id| UTType::typeWithIdentifier(&NSString::from_str(id))).collect();
    let types = NSArray::from_retained_slice(&types);
    let picker = UIDocumentPickerViewController::initForOpeningContentTypes_asCopy(UIDocumentPickerViewController::alloc(mtm), &types, true);
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    #[allow(deprecated)]
    let root = UIApplication::sharedApplication(mtm).keyWindow()?.rootViewController()?;
    // Whatever is already presented (a previous sheet) is the one to present from.
    let mut top = root;
    while let Some(next) = top.presentedViewController() {
        top = next;
    }
    top.presentViewController_animated_completion(&picker, true, None);
    DELEGATE.with(|d| *d.borrow_mut() = Some(delegate));
    Some(Box::pin(std::future::poll_fn(move |_| match answer.borrow_mut().take() {
        Some(a) => {
            DELEGATE.with(|d| *d.borrow_mut() = None);
            Poll::Ready(a)
        }
        None => Poll::Pending,
    })))
}
