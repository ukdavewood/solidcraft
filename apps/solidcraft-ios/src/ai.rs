//! Ask Apple's on-device language model (iPadOS 26 "Apple Intelligence") to run SolidCraft
//! commands. A spike: the model lives in a small Swift framework (`SolidCraftAI`, built by the
//! Xcode wrapper) that the app loads at run time; the framework calls back here, on a worker
//! thread, to look commands up, run them and inspect the design, and each request is answered on
//! the UI thread in [`State::tick`], where the design may be touched.
//!
//! Loading a framework and calling through its function pointers has no safe API, so this module
//! allows `unsafe` (as `picker` and `keyboard` do).
#![allow(unsafe_code)]
use std::ffi::{CStr, CString, c_char};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use solidcraft_ui_egui::SolveApp;

type ExecFn = extern "C" fn(*const c_char) -> *mut c_char;
type DoneFn = extern "C" fn(*const c_char);

/// Requests from the model's tools: the JSON text and where the answer goes.
static REQUESTS: Mutex<Vec<(String, mpsc::Sender<String>)>> = Mutex::new(Vec::new());
/// Finished answers (or errors) from the model.
static ANSWERS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// What the model's tools did, for the window (so a wrong turn can be seen).
static TRACE: Mutex<Vec<String>> = Mutex::new(Vec::new());
static CTX: OnceLock<egui::Context> = OnceLock::new();

/// The longest tool answer: the model's context is small.
const MAX_REPLY: usize = 1200;

fn repaint() {
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

extern "C" fn exec_cb(request: *const c_char) -> *mut c_char {
    // SAFETY: the framework passes a NUL-terminated string that lives for this call.
    let text = unsafe { CStr::from_ptr(request) }.to_string_lossy().into_owned();
    let (tx, rx) = mpsc::channel();
    if let Ok(mut q) = REQUESTS.lock() {
        q.push((text, tx));
    }
    repaint();
    let reply = rx.recv_timeout(Duration::from_secs(60)).unwrap_or_else(|_| "error: the app did not answer".to_string());
    let c = CString::new(reply.replace('\0', " ")).unwrap_or_default();
    // SAFETY: `strdup` copies into malloc memory, which the framework releases with `free`.
    unsafe { libc::strdup(c.as_ptr()) }
}

extern "C" fn done_cb(answer: *const c_char) {
    // SAFETY: as above.
    let text = unsafe { CStr::from_ptr(answer) }.to_string_lossy().into_owned();
    if let Ok(mut a) = ANSWERS.lock() {
        a.push(text);
    }
    repaint();
}

/// The framework's entry points.
struct Bridge {
    ask: extern "C" fn(*const c_char, DoneFn),
    reset: extern "C" fn(),
    availability: extern "C" fn() -> *mut c_char,
}

fn load() -> Result<Bridge, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let path = exe.parent().ok_or("no app folder")?.join("Frameworks/SolidCraftAI.framework/SolidCraftAI");
    let cpath = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: loading a framework of our own, which is signed into the app.
    let handle = unsafe { libc::dlopen(cpath.as_ptr(), libc::RTLD_NOW) };
    if handle.is_null() {
        // SAFETY: `dlerror` returns a C string or null.
        let why = unsafe { libc::dlerror() };
        let why = if why.is_null() { String::new() } else { unsafe { CStr::from_ptr(why) }.to_string_lossy().into_owned() };
        return Err(format!("cannot load {}: {why}", path.display()));
    }
    let sym = |name: &CStr| -> Result<*mut libc::c_void, String> {
        // SAFETY: `handle` is a live library handle.
        let p = unsafe { libc::dlsym(handle, name.as_ptr()) };
        if p.is_null() { Err(format!("missing {}", name.to_string_lossy())) } else { Ok(p) }
    };
    let set_exec = sym(c"sc_ai_set_exec")?;
    // SAFETY: the symbols are the `@_cdecl` functions of AIBridge.swift, with exactly these signatures.
    unsafe {
        let set_exec: extern "C" fn(ExecFn) = std::mem::transmute(set_exec);
        set_exec(exec_cb);
        Ok(Bridge {
            ask: std::mem::transmute(sym(c"sc_ai_ask")?),
            reset: std::mem::transmute(sym(c"sc_ai_reset")?),
            availability: std::mem::transmute(sym(c"sc_ai_availability")?),
        })
    }
}

fn clip(mut text: String) -> String {
    if text.len() > MAX_REPLY {
        let mut end = MAX_REPLY;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(" …(cut)");
    }
    text
}

/// Answer one tool request from the model.
fn handle(app: &mut SolveApp, request: &str) -> String {
    let reply = answer(app, request);
    if let Ok(mut t) = TRACE.lock() {
        t.push(format!("· {} → {}", request.chars().take(160).collect::<String>(), reply.chars().take(100).collect::<String>()));
    }
    reply
}

fn answer(app: &mut SolveApp, request: &str) -> String {
    let v: Value = serde_json::from_str(request).unwrap_or(Value::Null);
    match v.get("tool").and_then(Value::as_str) {
        Some("find") => {
            let query = v.get("query").and_then(Value::as_str).unwrap_or("").to_lowercase();
            // Everyday words for what the commands call something else.
            let mut words: Vec<String> = Vec::new();
            for w in query.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 1) {
                let mapped = match w {
                    "cube" | "block" | "cuboid" | "brick" | "square" | "rectangular" => "box",
                    "ball" | "globe" => "sphere",
                    "rod" | "tube" | "can" | "pillar" | "column" | "cylindrical" => "cylinder",
                    "round" | "rounded" | "rounding" => "fillet",
                    "bevel" => "chamfer",
                    "remove" | "erase" => "delete",
                    "dimension" | "size" | "value" => "parameters",
                    other => other,
                };
                words.push(mapped.to_string());
            }
            let listed = app.run("engine.commands", json!({})).unwrap_or(Value::Null);
            let all = listed.as_array().or_else(|| listed.get("commands").and_then(Value::as_array)).cloned().unwrap_or_default();
            // Matches in the id or label count more than matches in the parameter text.
            let mut hits: Vec<(std::cmp::Reverse<usize>, usize, String)> = all
                .iter()
                .filter_map(|c| {
                    let id = c.get("id")?.as_str()?;
                    let label = c.get("label").and_then(Value::as_str).unwrap_or("");
                    let params = c.get("params").and_then(Value::as_str).unwrap_or("");
                    let name = format!("{id} {label}").to_lowercase();
                    let params_lc = params.to_lowercase();
                    let score: usize = words.iter().map(|w| if name.contains(w.as_str()) { 3 } else { usize::from(params_lc.contains(w.as_str())) }).sum();
                    (score > 0).then(|| (std::cmp::Reverse(score), id.len(), format!("{id} | {label} | {}", params.chars().take(220).collect::<String>())))
                })
                .collect();
            hits.sort();
            hits.truncate(5);
            if hits.is_empty() {
                "no command matches; for shapes use solid.box, solid.cylinder or solid.sphere".to_string()
            } else {
                clip(hits.into_iter().map(|h| h.2).collect::<Vec<_>>().join("\n"))
            }
        }
        Some("run") => {
            let command = v.get("command").and_then(Value::as_str).unwrap_or("");
            let params = v.get("params").and_then(Value::as_str).and_then(|p| serde_json::from_str::<Value>(p).ok()).filter(Value::is_object);
            match app.run(command, params.unwrap_or_else(|| json!({}))) {
                Ok(r) => clip(r.to_string()),
                Err(e) => clip(format!("error: {e}")),
            }
        }
        Some("inspect") => {
            let state = app.session.world_state();
            let bodies: Vec<String> = state.bodies.iter().take(60).map(|b| format!("{}{}", b.name, if app.session.body_visible(&b.name) { "" } else { " (hidden)" })).collect();
            let params: Vec<String> = app.session.doc.params.iter().take(30).map(|p| format!("{}={} {}", p.name, p.expr, p.unit)).collect();
            clip(format!("bodies ({}): {}\nparameters: {}", state.bodies.len(), bodies.join(", "), params.join("; ")))
        }
        _ => "error: unknown request".to_string(),
    }
}

/// The AI box: a button, and a window to type a request in.
#[derive(Default)]
pub struct State {
    open: bool,
    input: String,
    log: Vec<String>,
    busy: bool,
    bridge: Option<Result<Bridge, String>>,
    availability: String,
}

impl State {
    fn bridge(&mut self) -> Result<&Bridge, String> {
        if self.bridge.is_none() {
            let loaded = load();
            if let Ok(b) = &loaded {
                let p = (b.availability)();
                if !p.is_null() {
                    // SAFETY: the framework returns a `strdup`ed C string.
                    self.availability = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
                    unsafe { libc::free(p.cast()) };
                }
            }
            self.bridge = Some(loaded);
        }
        match &self.bridge {
            Some(Ok(b)) => Ok(b),
            Some(Err(e)) => Err(e.clone()),
            None => Err("not loaded".to_string()),
        }
    }

    /// Answer the model's requests and take in its answers (the UI thread, once a frame).
    pub fn tick(&mut self, app: &mut SolveApp) {
        let requests: Vec<_> = REQUESTS.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for (text, tx) in requests {
            let reply = handle(app, &text);
            let _ = tx.send(reply);
        }
        for line in TRACE.lock().map(|mut t| std::mem::take(&mut *t)).unwrap_or_default() {
            self.log.push(line);
        }
        let answers: Vec<_> = ANSWERS.lock().map(|mut a| std::mem::take(&mut *a)).unwrap_or_default();
        for a in answers {
            self.busy = false;
            self.log.push(format!("AI: {a}"));
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let _ = CTX.set(ctx.clone());
        egui::Area::new(egui::Id::new("ai_button")).anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -96.0]).show(ctx, |ui| {
            if ui.add_sized([56.0, 44.0], egui::Button::new("AI")).clicked() {
                self.open = !self.open;
            }
        });
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("AI (on-device)").open(&mut open).default_width(420.0).anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -150.0]).show(ctx, |ui| {
            let status = match self.bridge() {
                Ok(_) => self.availability.clone(),
                Err(e) => format!("not available: {e}"),
            };
            ui.label(egui::RichText::new(status).weak());
            egui::ScrollArea::vertical().max_height(260.0).stick_to_bottom(true).show(ui, |ui| {
                for line in &self.log {
                    ui.label(line);
                }
                if self.busy {
                    ui.label("…thinking");
                }
            });
            ui.separator();
            ui.add(egui::TextEdit::multiline(&mut self.input).desired_rows(2).desired_width(f32::INFINITY).hint_text("Ask SolidCraft to do something"));
            ui.horizontal(|ui| {
                let ready = !self.busy && !self.input.trim().is_empty() && self.availability == "available";
                if ui.add_enabled(ready, egui::Button::new("Ask")).clicked() {
                    let prompt = std::mem::take(&mut self.input);
                    self.log.push(format!("You: {prompt}"));
                    self.busy = true;
                    if let (Ok(b), Ok(c)) = (self.bridge(), CString::new(prompt.replace('\0', " "))) {
                        (b.ask)(c.as_ptr(), done_cb);
                    }
                }
                if ui.button("New chat").clicked() {
                    self.log.clear();
                    if let Ok(b) = self.bridge() {
                        (b.reset)();
                    }
                }
            });
        });
        self.open = open;
    }
}
