//! Open and Save on iPad without native calls: an in-app browser of the app's Documents folder.
//!
//! The folder is shared with the Files app (`UIFileSharingEnabled`: Files > On My iPad >
//! SolidCraft), so designs reach the app through Files, AirDrop and iCloud Drive copies, and what
//! the app saves is there to be moved on. The system document picker (which needs Objective-C
//! calls) replaces this later.
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::task::Poll;

use solidcraft_ui_egui::{FilePickerFuture, Services};

/// What the browser is asked for.
enum Request {
    Open,
    Save { name: String },
}

#[derive(Default)]
struct State {
    request: Option<Request>,
    /// Extensions offered (lower case, no dot); empty: every file.
    exts: Vec<String>,
    /// The answer, once given: `Some(None)` is a cancel.
    answer: Option<Option<String>>,
}

/// The browser: shared between the services the app calls and the window drawn each frame.
#[derive(Clone, Default)]
pub struct Files {
    state: Rc<RefCell<State>>,
}

/// The app's Documents folder (a sibling of its `tmp` folder in the container).
pub fn documents() -> PathBuf {
    let dir = std::env::temp_dir().parent().map(|p| p.join("Documents")).unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn listing(dir: &Path, exts: &[String]) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .filter(|n| {
            exts.is_empty()
                || Path::new(n).extension().and_then(|x| x.to_str()).is_some_and(|x| exts.iter().any(|e| e.eq_ignore_ascii_case(x)))
        })
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

impl Files {
    fn ask(&self, request: Request, exts: &[&str]) -> FilePickerFuture {
        {
            let mut s = self.state.borrow_mut();
            s.request = Some(request);
            s.exts = exts.iter().map(|e| e.trim_start_matches('.').to_lowercase()).collect();
            s.answer = None;
        }
        let state = self.state.clone();
        Box::pin(std::future::poll_fn(move |_| match state.borrow_mut().answer.take() {
            Some(answer) => Poll::Ready(answer),
            None => Poll::Pending,
        }))
    }

    /// The system picker where there is one; else the in-app browser.
    fn open_or_browse(&self, exts: &[&str]) -> FilePickerFuture {
        #[cfg(target_os = "ios")]
        if let Some(future) = crate::picker::open() {
            return future;
        }
        self.ask(Request::Open, exts)
    }

    /// The services the app asks the host for.
    pub fn services(&self) -> Services {
        let (open, save, file) = (self.clone(), self.clone(), self.clone());
        Services {
            pick_open: Some(Box::new(move |exts| open.open_or_browse(exts))),
            pick_save: Some(Box::new(move |name, exts| save.ask(Request::Save { name: name.to_owned() }, exts))),
            pick_file: Some(Box::new(move |_, exts| file.open_or_browse(exts))),
            ..Services::default()
        }
    }

    /// Draw the browser while a request is open.
    pub fn show(&self, ctx: &egui::Context) {
        let mut s = self.state.borrow_mut();
        let exts = s.exts.clone();
        let Some(request) = s.request.as_mut() else { return };
        let dir = documents();
        let mut done: Option<Option<String>> = None;
        let title = if matches!(request, Request::Save { .. }) { "Save to Files" } else { "Open from Files" };
        egui::Window::new(title).collapsible(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.label("Files > On My iPad > SolidCraft");
            ui.separator();
            let names = listing(&dir, &exts);
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                if names.is_empty() {
                    ui.label("No files yet. Copy designs into the SolidCraft folder in the Files app.");
                }
                for n in &names {
                    if ui.add_sized([ui.available_width(), 44.0], egui::Button::new(n).truncate()).clicked() {
                        match request {
                            Request::Open => done = Some(Some(dir.join(n).to_string_lossy().into_owned())),
                            Request::Save { name } => *name = n.clone(),
                        }
                    }
                }
            });
            if let Request::Save { name } = request {
                ui.separator();
                ui.text_edit_singleline(name);
                if ui.add_enabled(!name.trim().is_empty(), egui::Button::new("Save")).clicked() {
                    let mut file = name.trim().to_owned();
                    if let Some(ext) = exts.first().filter(|e| Path::new(&file).extension().is_none_or(|x| !x.to_string_lossy().eq_ignore_ascii_case(e))) {
                        file = format!("{file}.{ext}");
                    }
                    done = Some(Some(dir.join(file).to_string_lossy().into_owned()));
                }
            }
            if ui.button("Cancel").clicked() {
                done = Some(None);
            }
        });
        if done.is_some() {
            s.request = None;
            s.answer = done;
        }
    }
}
