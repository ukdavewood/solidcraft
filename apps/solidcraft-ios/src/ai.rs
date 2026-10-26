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
type TextFn = extern "C" fn(*const c_char, i32);

/// Requests from the model's tools: the JSON text and where the answer goes.
static REQUESTS: Mutex<Vec<(String, mpsc::Sender<String>)>> = Mutex::new(Vec::new());
/// Finished answers (or errors) from the model.
static ANSWERS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// What the model's tools did, for the window (so a wrong turn can be seen).
static TRACE: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Words from the microphone: the text and its kind (0 so far, 1 final, 2 an error).
static SPOKEN: Mutex<Vec<(String, i32)>> = Mutex::new(Vec::new());
static CTX: OnceLock<egui::Context> = OnceLock::new();

/// The longest tool answer: the model's context is small.
const MAX_REPLY: usize = 600;

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

extern "C" fn spoken_cb(text: *const c_char, kind: i32) {
    // SAFETY: as above.
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned();
    if let Ok(mut s) = SPOKEN.lock() {
        s.push((text, kind));
    }
    repaint();
}

/// The framework's entry points.
struct Bridge {
    ask: extern "C" fn(*const c_char, DoneFn),
    reset: extern "C" fn(),
    availability: extern "C" fn() -> *mut c_char,
    listen_start: extern "C" fn(TextFn),
    listen_stop: extern "C" fn(),
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
            listen_start: std::mem::transmute(sym(c"sc_ai_listen_start")?),
            listen_stop: std::mem::transmute(sym(c"sc_ai_listen_stop")?),
        })
    }
}

/// A command's result in few words: what it made or changed, else its start.
fn short_result(r: &Value) -> String {
    let list = |k: &str| r.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    let (made, changed) = (list("created_bodies"), list("changed_bodies"));
    if made.is_empty() && changed.is_empty() { clip(format!("ok {r}")) } else { format!("ok; created: [{made}]; changed: [{changed}]") }
}

/// Fillet or chamfer the chosen edges of a body in one step: the model names the kind, the size and
/// which edges; the edges themselves are picked here (it cannot see them).
fn round_edges(app: &mut SolveApp, v: &Value) -> String {
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
    let size = v.get("size").and_then(Value::as_f64).filter(|s| s.is_finite() && *s > 0.0);
    let Some(size) = size else { return "error: size must be a positive number of millimetres".to_string() };
    // A named body when there is one; "newest", "last", an empty or an unknown name mean the newest.
    let named = text("body");
    let state = app.session.world_state();
    let body = state.bodies.iter().find(|x| !named.is_empty() && x.name.to_lowercase() == named).or_else(|| state.bodies.iter().rev().find(|x| app.session.body_visible(&x.name))).or(state.bodies.last()).map(|x| x.name.clone());
    let Some(body) = body else { return "error: no such body (use inspect_design for the names)".to_string() };
    let listed = match app.run("model.edges", json!({ "body": body })) {
        Ok(l) => l,
        Err(e) => return clip(format!("error: {e}")),
    };
    let edges = listed.get("edges").and_then(Value::as_array).cloned().unwrap_or_default();
    let pt = |e: &Value, k: &str| -> Option<[f64; 3]> {
        let a = e.get(k)?.as_array()?;
        Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?])
    };
    let zs: Vec<f64> = edges.iter().filter_map(|e| pt(e, "mid").map(|m| m[2])).collect();
    let (zmin, zmax) = (zs.iter().cloned().fold(f64::INFINITY, f64::min), zs.iter().cloned().fold(f64::NEG_INFINITY, f64::max));
    let tol = 1e-6 * (1.0 + zmax.abs().max(zmin.abs()));
    let which = text("which");
    let picked: Vec<Value> = edges
        .iter()
        .filter(|e| {
            let (Some(a), Some(b)) = (pt(e, "start"), pt(e, "end")) else { return false };
            let flat = (a[2] - b[2]).abs() <= tol;
            let vertical = (a[0] - b[0]).abs() <= tol && (a[1] - b[1]).abs() <= tol;
            match which.as_str() {
                "top" => flat && (a[2] - zmax).abs() <= tol,
                "bottom" => flat && (a[2] - zmin).abs() <= tol,
                "vertical" | "sides" => vertical,
                _ => true,
            }
        })
        .filter_map(|e| Some(json!({ "body": body, "index": e.get("index")?.as_u64()? })))
        .collect();
    if picked.is_empty() {
        return "error: no edges match".to_string();
    }
    let chamfer = text("kind").starts_with("cham") || text("kind").starts_with("bevel");
    let (cmd, params) = if chamfer {
        ("solid.chamfer", json!({ "edges": picked, "distance": size, "body": body }))
    } else {
        ("solid.fillet", json!({ "edges": picked, "radius": size, "body": body }))
    };
    match app.run(cmd, params) {
        Ok(_) => format!("ok: {} {} edges of {body} by {size} mm", if chamfer { "chamfered" } else { "filleted" }, picked.len()),
        Err(e) => clip(format!("error: {e}")),
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
                    "clear" | "empty" | "reset" | "wipe" | "restart" => "new design",
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
            // Parameters arrive as a JSON object, or as JSON text: a small model writes the text
            // badly, so `view.show_hide` also takes whatever follows `rule:`.
            let params = match v.get("params") {
                Some(o) if o.is_object() => Some(o.clone()),
                Some(t) => t.as_str().and_then(|p| serde_json::from_str::<Value>(p).ok()).filter(Value::is_object).or_else(|| {
                    let text = t.as_str()?.trim();
                    (command == "view.show_hide" && !text.is_empty())
                        .then(|| json!({ "rule": text.trim_start_matches("rule").trim_start_matches(':').trim().trim_matches('"') }))
                }),
                None => None,
            };
            // Starting or opening another design drops what is on screen: not while it holds
            // unsaved changes (the user saves first, or says so, and the app's own New asks).
            if ["file.new", "doc.open"].contains(&command) && app.session.is_dirty() {
                return "refused: the open design has unsaved changes. Tell the user to save it first (File > Save) or to start a new design with the File menu, which asks.".to_string();
            }
            match app.run(command, params.unwrap_or_else(|| json!({}))) {
                Ok(r) => short_result(&r),
                Err(e) => clip(format!("error: {e}")),
            }
        }
        Some("round") => round_edges(app, &v),
        Some("shape") => {
            let n = |k: &str| v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite() && *x > 0.0);
            let kind = v.get("kind").and_then(Value::as_str).unwrap_or("").to_lowercase();
            let (cmd, params) = match kind.as_str() {
                k if ["box", "cube", "block", "cuboid"].contains(&k) => {
                    let (l, w, h) = (n("a"), n("b").or(n("a")), n("c").or(n("a")));
                    let (Some(l), Some(w), Some(h)) = (l, w, h) else { return "error: a box needs a size in millimetres".to_string() };
                    ("solid.box", json!({ "length": l, "width": w, "height": h }))
                }
                k if ["cylinder", "rod", "tube"].contains(&k) => {
                    let (Some(r), Some(h)) = (n("a"), n("b")) else { return "error: a cylinder needs a radius and a height in millimetres".to_string() };
                    ("solid.cylinder", json!({ "radius": r, "height": h }))
                }
                k if ["sphere", "ball"].contains(&k) => {
                    let Some(r) = n("a") else { return "error: a sphere needs a radius in millimetres".to_string() };
                    ("solid.sphere", json!({ "radius": r }))
                }
                _ => return "error: kind must be box, cylinder or sphere".to_string(),
            };
            match app.run(cmd, params) {
                Ok(r) => short_result(&r),
                Err(e) => clip(format!("error: {e}")),
            }
        }
        Some("inspect") => {
            let state = app.session.world_state();
            let bodies: Vec<String> = state.bodies.iter().take(60).map(|b| format!("{}{}", b.name, if app.session.body_visible(&b.name) { "" } else { " (hidden)" })).collect();
            let params: Vec<String> = app.session.doc.params.iter().take(30).map(|p| format!("{}={}", p.name, if p.unit.is_empty() || p.expr.contains(p.unit.as_str()) { p.expr.clone() } else { format!("{} {}", p.expr, p.unit) })).collect();
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
    listening: bool,
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

    /// Send what is typed to the model.
    fn submit(&mut self) {
        let prompt = std::mem::take(&mut self.input);
        self.log.push(format!("You: {prompt}"));
        self.busy = true;
        if let Ok(b) = self.bridge() {
            // Each request starts clean: the design is the memory, and the model's own is small.
            (b.reset)();
            if let Ok(c) = CString::new(prompt.trim().replace('\0', " ")) {
                (b.ask)(c.as_ptr(), done_cb);
            }
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
        for (text, kind) in SPOKEN.lock().map(|mut s| std::mem::take(&mut *s)).unwrap_or_default() {
            match kind {
                2 => {
                    self.listening = false;
                    self.log.push(text);
                }
                1 => {
                    self.listening = false;
                    self.input = text;
                }
                _ => self.input = text,
            }
        }
        let answers: Vec<_> = ANSWERS.lock().map(|mut a| std::mem::take(&mut *a)).unwrap_or_default();
        for a in answers {
            self.busy = false;
            self.log.push(format!("AI: {a}"));
        }
    }

    /// `lift`: how far a docked keyboard reaches up from the bottom of the screen (0: none).
    pub fn show(&mut self, ctx: &egui::Context, lift: f32) {
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
        egui::Window::new("AI (on-device)").open(&mut open).default_width(420.0).anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, if lift > 0.0 { -(lift + 12.0) } else { -150.0 }]).show(ctx, |ui| {
            let status = match self.bridge() {
                Ok(_) => self.availability.clone(),
                Err(e) => format!("not available: {e}"),
            };
            ui.label(egui::RichText::new(if self.listening { format!("{status} · listening…") } else { status }).weak());
            egui::ScrollArea::vertical().max_height(260.0).stick_to_bottom(true).show(ui, |ui| {
                for line in &self.log {
                    ui.label(line);
                }
                if self.busy {
                    ui.label("…thinking");
                }
            });
            ui.separator();
            // Enter asks; Shift+Enter is a new line.
            let id = egui::Id::new("ai_input");
            let enter = ui.memory(|m| m.has_focus(id)) && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            ui.add(egui::TextEdit::multiline(&mut self.input).id(id).desired_rows(2).desired_width(f32::INFINITY).hint_text("Ask SolidCraft to do something"));
            ui.horizontal(|ui| {
                let ready = !self.busy && !self.input.trim().is_empty() && self.availability == "available";
                if (ui.add_enabled(ready, egui::Button::new("Ask")).clicked() || (enter && ready)) && ready {
                    self.submit();
                }
                let speak = if self.listening { "Stop" } else { "Speak" };
                if ui.button(speak).clicked() {
                    let was_listening = self.listening;
                    if let Ok(b) = self.bridge() {
                        if was_listening {
                            (b.listen_stop)();
                        } else {
                            (b.listen_start)(spoken_cb);
                        }
                        self.listening = !was_listening;
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
