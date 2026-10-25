//! The system document picker (the Files sheet: iCloud Drive, On My iPad, other apps' folders,
//! subfolders, AirDrop'd files) for Open.
//!
//! UIKit has no safe Rust API, so this module is the one place that allows `unsafe`: it calls
//! `UIDocumentPickerViewController` through the `objc2` bindings.
//!
//! A picked file is opened *in place*: the app is granted access to the original (iCloud Drive,
//! another folder), keeps that access, and works on a copy in its own Documents folder, so the
//! rest of the app opens and saves an ordinary path. After each save of the copy, [`sync`] writes
//! it back over the original.
#![allow(unsafe_code)]
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;
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
            let path = urls.firstObject().and_then(|url| {
                // Access to the original lasts as long as the app keeps it.
                // SAFETY: balanced by never stopping: the grant is held for the app's lifetime.
                let _granted = unsafe { url.startAccessingSecurityScopedResource() };
                let local = url.path().and_then(|p| import(&p.to_string()));
                SCOPED.with(|s| s.borrow_mut().push(url.clone()));
                local
            });
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

/// A working copy and the original it was opened from.
struct Link {
    local: PathBuf,
    original: PathBuf,
    /// The copy's modification time when it was last written back (or imported).
    synced: SystemTime,
}

thread_local! {
    static LINKS: RefCell<Vec<Link>> = const { RefCell::new(Vec::new()) };
    /// The URLs whose access grants the app keeps.
    static SCOPED: RefCell<Vec<Retained<NSURL>>> = const { RefCell::new(Vec::new()) };
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
    let picker = UIDocumentPickerViewController::initForOpeningContentTypes_asCopy(UIDocumentPickerViewController::alloc(mtm), &types, false);
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

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Copy the picked file into the app's Documents folder and remember where it came from.
fn import(original: &str) -> Option<String> {
    let name = Path::new(original).file_name()?;
    let local = crate::files::documents().join(name);
    std::fs::copy(original, &local).ok()?;
    let synced = modified(&local)?;
    LINKS.with(|l| {
        let mut l = l.borrow_mut();
        l.retain(|k| k.local != local);
        l.push(Link { local: local.clone(), original: PathBuf::from(original), synced });
    });
    Some(local.to_string_lossy().into_owned())
}

/// Whether any open file has an original to write back to.
pub fn linked() -> bool {
    LINKS.with(|l| !l.borrow().is_empty())
}

/// Write every working copy saved since the last call back over its original. The messages are
/// for the status line: (text, is an error).
pub fn sync() -> Vec<(String, bool)> {
    let mut messages = Vec::new();
    LINKS.with(|l| {
        for k in l.borrow_mut().iter_mut() {
            let Some(m) = modified(&k.local) else { continue };
            if m <= k.synced {
                continue;
            }
            k.synced = m;
            let name = k.original.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            messages.push(match std::fs::copy(&k.local, &k.original) {
                Ok(_) => (format!("Saved {name} back to its original location"), false),
                Err(e) => (format!("Could not save {name} back to its original location: {e}"), true),
            });
        }
    });
    messages
}
