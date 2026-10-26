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

use crate::ai_data::{self, Hints};

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
/// The tool calls of the request being answered, as sent and as answered, for the log.
static CALLS: Mutex<Vec<(Value, String)>> = Mutex::new(Vec::new());
/// What the on-device model was asked (normalised), while it is working: it may not start a new design the user did not ask for.
static APPLE_REQUEST: Mutex<Option<String>> = Mutex::new(None);
/// Every call of the request being answered, for the loop guard (CALLS is emptied into the log each frame).
static GUARD: Mutex<Vec<(Value, String)>> = Mutex::new(Vec::new());
/// Progress and results of the local model's download: text and a kind (0 progress, 1 ready, 2 error).
static LOCAL: Mutex<Vec<(String, i32)>> = Mutex::new(Vec::new());
static CTX: OnceLock<egui::Context> = OnceLock::new();

/// Words that end speaking (or cancel a request) instead of being sent as one.
const STOP_WORDS: &[&str] = &["stop", "stop listening", "cancel", "never mind", "nevermind", "that's all", "thats all", "enough"];

/// The Claude model asked when the on-device model can't (a different one: `claude_model` in the hints file).
const CLAUDE_MODEL: &str = "claude-haiku-5-5";

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

extern "C" fn local_cb(text: *const c_char, kind: i32) {
    // SAFETY: as above.
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned();
    if let Ok(mut s) = LOCAL.lock() {
        s.push((text, kind));
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
    claude_ask: extern "C" fn(*const c_char, *const c_char, DoneFn),
    has_key: extern "C" fn() -> i32,
    set_key: extern "C" fn(*const c_char) -> i32,
    clear_key: extern "C" fn(),
    usage: extern "C" fn() -> *mut c_char,
    paste: extern "C" fn() -> *mut c_char,
    local_state: extern "C" fn() -> *mut c_char,
    local_load: extern "C" fn(TextFn),
    local_unload: extern "C" fn(),
    local_ask: extern "C" fn(*const c_char, DoneFn),
    online: extern "C" fn() -> i32,
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
            claude_ask: std::mem::transmute(sym(c"sc_ai_claude_ask")?),
            has_key: std::mem::transmute(sym(c"sc_ai_has_key")?),
            set_key: std::mem::transmute(sym(c"sc_ai_set_key")?),
            clear_key: std::mem::transmute(sym(c"sc_ai_clear_key")?),
            usage: std::mem::transmute(sym(c"sc_ai_claude_usage")?),
            paste: std::mem::transmute(sym(c"sc_ai_paste")?),
            local_state: std::mem::transmute(sym(c"sc_ai_local_state")?),
            local_load: std::mem::transmute(sym(c"sc_ai_local_load")?),
            local_unload: std::mem::transmute(sym(c"sc_ai_local_unload")?),
            local_ask: std::mem::transmute(sym(c"sc_ai_local_ask")?),
            online: std::mem::transmute(sym(c"sc_ai_online")?),
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

fn clip(text: String) -> String {
    clip_to(text, MAX_REPLY)
}

fn clip_to(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
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
    let reply = loop_guard(request).unwrap_or_else(|| answer(app, request));
    if let (Ok(mut g), Ok(call)) = (GUARD.lock(), serde_json::from_str::<Value>(request)) {
        g.push((call, reply.clone()));
    }
    if let (Ok(mut calls), Ok(call)) = (CALLS.lock(), serde_json::from_str::<Value>(request)) {
        calls.push((call, reply.clone()));
    }
    if let Ok(mut t) = TRACE.lock() {
        t.push(format!("· {} → {}", request.chars().take(160).collect::<String>(), reply.chars().take(100).collect::<String>()));
    }
    reply
}

/// Stops a model that keeps repeating itself: after a hand-over (`escalate`), after the same call
/// failed once already, or after too many calls, nothing more runs and the model is told to finish.
fn loop_guard(request: &str) -> Option<String> {
    const STOP: &str = "STOP: do not call any more tools. Reply with one short sentence.";
    let calls = GUARD.lock().ok()?;
    let tool = |c: &Value| c.get("tool").and_then(Value::as_str).unwrap_or("").to_string();
    let this: Value = serde_json::from_str(request).ok()?;
    if calls.iter().any(|(c, _)| tool(c) == "escalate") && tool(&this) != "escalate" {
        return Some(STOP.to_string());
    }
    if calls.len() >= 30 {
        return Some(STOP.to_string());
    }
    if !matches!(tool(&this).as_str(), "inspect" | "find") && calls.iter().any(|(c, r)| *c == this && (r.starts_with("error") || r.starts_with("refused"))) {
        return Some(format!("{STOP} This exact call already failed."));
    }
    None
}

/// Moves the camera: pan, zoom, rotate or a standard view. The model does not change.
fn change_view(app: &mut SolveApp, v: &Value) -> String {
    use solidcraft_engine::render::StandardView;
    let action = v.get("action").and_then(Value::as_str).unwrap_or("").trim().to_lowercase().replace([' ', '-'], "_");
    // A negative amount goes the other way.
    let signed = v.get("amount").and_then(Value::as_f64).filter(|a| a.is_finite() && *a != 0.0);
    let sign = if signed.is_some_and(|a| a < 0.0) { -1.0 } else { 1.0 };
    let amount = signed.map(f64::abs);
    // The view's height only scales a pan, so a nominal one gives a pan in screen pixels.
    const H: f64 = 800.0;
    let pan = amount.unwrap_or(200.0).min(2000.0);
    let degrees = amount.unwrap_or(45.0).min(360.0);
    // Camera::orbit turns 0.008 radians per pixel.
    let px = degrees.to_radians() / 0.008;
    let zoom = amount.filter(|a| *a > 1.0).unwrap_or(2.0).min(20.0);
    app.cam_anim = None;
    match action.as_str() {
        // The model moves the way the word says.
        "pan_left" => app.cam.pan(-sign * pan, 0.0, H),
        "pan_right" => app.cam.pan(sign * pan, 0.0, H),
        "pan_up" => app.cam.pan(0.0, -sign * pan, H),
        "pan_down" => app.cam.pan(0.0, sign * pan, H),
        "zoom_in" => app.cam.zoom_at(1.0 / zoom, None),
        "zoom_out" => app.cam.zoom_at(zoom, None),
        "rotate_left" | "orbit_left" | "turn_left" => app.cam.orbit(-sign * px, 0.0),
        "rotate_right" | "orbit_right" | "turn_right" => app.cam.orbit(sign * px, 0.0),
        "rotate_up" | "orbit_up" | "tilt_up" => app.cam.orbit(0.0, sign * px),
        "rotate_down" | "orbit_down" | "tilt_down" => app.cam.orbit(0.0, -sign * px),
        // The same moves as the view buttons: animated, and Home is the isometric view fitted.
        "home" | "home_view" => app.animate_view("home"),
        "fit" | "zoom_to_fit" | "zoom_extents" => app.animate_view("fit"),
        other => {
            let name = if other == "isometric" { "iso" } else { other.trim_end_matches("_view") };
            if StandardView::parse(name).is_none() {
                return "error: action must be pan_left, pan_right, pan_up, pan_down, zoom_in, zoom_out, rotate_left, rotate_right, tilt_up, tilt_down, front, back, left, right, top, bottom, isometric, home or fit".to_string();
            }
            app.animate_view(name);
        }
    }
    format!("ok: view {action}")
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
            // A new design opens in its own tab: the one on screen stays open, so nothing is lost.
            if command == "file.new" {
                let asked = APPLE_REQUEST.lock().ok().and_then(|a| a.clone());
                if asked.is_some_and(|r| !r.split(' ').any(|w| matches!(w, "new" | "clear" | "reset" | "start" | "restart" | "empty" | "blank" | "fresh" | "wipe"))) {
                    return "refused: the user did not ask for a new design. Do not call again.".to_string();
                }
                if app.session.is_dirty() || !app.session.world_state().bodies.is_empty() {
                    solidcraft_ui_egui::documents::new_tab(app);
                    return "ok: a new design opened in a new tab; the previous design is still open in its own tab. If the user asked for nothing else, call no more tools and reply in one sentence.".to_string();
                }
                return "ok: the design on screen is already empty".to_string();
            }
            if command == "doc.open" && app.session.is_dirty() {
                return "refused: the open design has unsaved changes. Tell the user to open the file from the File menu, which opens it in a new tab. Do not call again.".to_string();
            }
            match app.run(command, params.unwrap_or_else(|| json!({}))) {
                Ok(r) => short_result(&r),
                Err(e) => clip(format!("error: {e}")),
            }
        }
        Some("view") => change_view(app, &v),
        Some("escalate") => "no tool can do this request: it needs a more capable assistant. Call no more tools and reply \"Handing over.\"".to_string(),
        Some("round") => round_edges(app, &v),
        Some("move") => {
            let n = |k: &str| v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite()).unwrap_or(0.0);
            let named = v.get("body").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
            let state = app.session.world_state();
            let body = state
                .bodies
                .iter()
                .find(|x| !named.is_empty() && x.name.to_lowercase() == named)
                .or_else(|| state.bodies.iter().rev().find(|x| app.session.body_visible(&x.name)))
                .or(state.bodies.last())
                .map(|x| x.name.clone());
            let Some(body) = body else { return "error: no body to move".to_string() };
            match app.run("solid.move", json!({ "bodies": [body], "translate": [n("x"), n("y"), n("z")] })) {
                Ok(_) => format!("ok: moved {body} by [{}, {}, {}] mm", n("x"), n("y"), n("z")),
                Err(e) => clip(format!("error: {e}")),
            }
        }
        Some("shape") => {
            // The on-device model adds a cube for "move the cube": shapes only come from add/create/make.
            let asked = APPLE_REQUEST.lock().ok().and_then(|a| a.clone());
            if asked.is_some_and(|r| !r.split(' ').any(|w| matches!(w, "add" | "create" | "make" | "build" | "new" | "draw" | "insert" | "place" | "put" | "another"))) {
                return "refused: the user did not ask to add a shape. To move, round or change something, use the tool for that on the existing body (inspect_design lists the bodies). Do not call again.".to_string();
            }
            let n = |k: &str| v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite() && *x > 0.0);
            let kind = v.get("kind").and_then(Value::as_str).unwrap_or("").to_lowercase();
            let (cmd, params) = match kind.as_str() {
                k if ["box", "cube", "block", "cuboid"].contains(&k) => {
                    // A cube has one size; a box with only one size is a cube too.
                    let cube = k == "cube";
                    let (l, w, h) = (n("a"), if cube { n("a") } else { n("b").or(n("a")) }, if cube { n("a") } else { n("c").or(n("a")) });
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
            let mut params = params;
            // Optional position, and join or cut against an existing body, in the same step.
            let at = |k: &str| v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite()).unwrap_or(0.0);
            let position = json!([at("x"), at("y"), at("z")]);
            if let Some(o) = params.as_object_mut() {
                let key = match cmd {
                    "solid.box" => "corner",
                    "solid.cylinder" => "base",
                    _ => "center",
                };
                if position != json!([0.0, 0.0, 0.0]) {
                    o.insert(key.to_string(), position);
                }
            }
            let operation = v.get("operation").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
            if ["join", "cut", "intersect"].contains(&operation.as_str()) {
                let named = v.get("target").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
                let state = app.session.world_state();
                let target = state
                    .bodies
                    .iter()
                    .find(|x| !named.is_empty() && x.name.to_lowercase() == named)
                    .or_else(|| state.bodies.iter().rev().find(|x| app.session.body_visible(&x.name)))
                    .map(|x| x.name.clone());
                let Some(target) = target else { return "error: no body to join to or cut".to_string() };
                if let Some(o) = params.as_object_mut() {
                    o.insert("operation".to_string(), json!(operation));
                    o.insert("targets".to_string(), json!([target]));
                }
            }
            match app.run(cmd, params) {
                Ok(r) => short_result(&r),
                Err(e) => clip(format!("error: {e}")),
            }
        }
        Some("inspect") => {
            let state = app.session.world_state();
            // Each body's extent, so a request like "beside the plate" can be worked out.
            let extents: std::collections::HashMap<String, String> = app
                .run("component.properties", json!({}))
                .ok()
                .and_then(|p| p.get("bodies").and_then(Value::as_array).cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|b| {
                    let name = b.get("name")?.as_str()?.to_string();
                    let (lo, hi) = (b.pointer("/bbox/min")?.as_array()?, b.pointer("/bbox/max")?.as_array()?);
                    let axis = |i: usize, n: &str| Some(format!("{n} {:.1}..{:.1}", lo.get(i)?.as_f64()?, hi.get(i)?.as_f64()?));
                    Some((name, format!(" [{}, {}, {}]", axis(0, "x")?, axis(1, "y")?, axis(2, "z")?)))
                })
                .collect();
            let bodies: Vec<String> = state
                .bodies
                .iter()
                .take(40)
                .map(|b| format!("{}{}{}", b.name, extents.get(&b.name).map(String::as_str).unwrap_or(""), if app.session.body_visible(&b.name) { "" } else { " (hidden)" }))
                .collect();
            // The timeline: feature names and kinds, so a feature can be inspected or edited by name.
            let features: Vec<String> = app
                .run("document.inspect", json!({}))
                .ok()
                .and_then(|d| d.get("timeline").and_then(Value::as_array).cloned())
                .unwrap_or_default()
                .iter()
                .take(40)
                .filter_map(|f| Some(format!("{}({})", f.get("name")?.as_str()?, f.get("type")?.as_str()?.trim_end_matches("Feature"))))
                .collect();
            // The parameters, and which feature input each one drives.
            let params: Vec<String> = app
                .run("parameters.list", json!({}))
                .ok()
                .and_then(|p| p.get("parameters").and_then(Value::as_array).cloned())
                .unwrap_or_default()
                .iter()
                .take(40)
                .filter_map(|q| {
                    let used = match (q.get("feature").and_then(Value::as_str), q.get("input").and_then(Value::as_str)) {
                        (Some(f), Some(i)) => format!(" [{f}.{i}]"),
                        _ => String::new(),
                    };
                    Some(format!("{}={}{used}", q.get("name")?.as_str()?, q.get("expression")?.as_str()?))
                })
                .collect();
            clip_to(
                format!("bodies ({}): {}\nfeatures: {}\nparameters: {}", state.bodies.len(), bodies.join(", "), features.join(" "), params.join("; ")),
                1800,
            )
        }
        _ => "error: unknown request".to_string(),
    }
}

/// The AI box: a button, and a window to type a request in.
/// The request being answered.
struct Current {
    id: u64,
    request: String,
    tier: &'static str,
    /// What the on-device attempt did before Claude was asked (told to Claude).
    earlier: String,
    calls: Vec<(Value, String)>,
}

/// The last request, waiting to be rated.
struct Rated {
    id: u64,
    request: String,
    tier: &'static str,
    calls: Vec<Value>,
    /// Every call went through (nothing refused or failed).
    clean: bool,
}

#[derive(Default)]
pub struct State {
    hints: Hints,
    /// The last request and its answer, told to Claude so a follow-up like "continue" makes sense.
    last_exchange: Option<(String, String)>,
    current: Option<Current>,
    /// Calls from a learned request or recipe, run on the next frame.
    pending: Option<Vec<Value>>,
    last: Option<Rated>,
    open: bool,
    input: String,
    log: Vec<String>,
    busy: bool,
    listening: bool,
    /// The local model: "none", "loading" or "ready"; and the download's progress text.
    local_state: String,
    local_progress: String,
    /// What was typed before Speak (the recogniser reports the whole phrase each time), when words last arrived (ms), and the user's choice not to send after a pause.
    speech_base: String,
    heard_at: u64,
    no_auto_send: bool,
    /// Speak keeps listening for the next command until Stop, typing or Paste.
    continuous: bool,
    /// The Claude tier: a key is saved; the user turned automatic hand-over off; the key being typed.
    has_key: bool,
    claude_off: bool,
    key_input: String,
    usage: String,
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

    /// Start the microphone (the words so far are kept as the start of the next phrase).
    fn start_listening(&mut self) {
        let base = if self.input.trim().is_empty() { String::new() } else { format!("{} ", self.input.trim_end()) };
        if let Ok(b) = self.bridge() {
            (b.listen_start)(spoken_cb);
            self.listening = true;
            self.speech_base = base;
            self.heard_at = ai_data::now_millis();
        }
    }

    /// Speech ends where typing, Paste or Stop begins.
    fn stop_speaking(&mut self) {
        self.continuous = false;
        if self.listening {
            self.listening = false;
            if let Ok(b) = self.bridge() {
                (b.listen_stop)();
            }
        }
    }

    /// Send what was said, then listen for the next command.
    fn submit_spoken(&mut self) {
        if STOP_WORDS.contains(&ai_data::normalise(&self.input).as_str()) {
            self.input.clear();
            self.stop_speaking();
            self.log.push("· Stopped listening".to_string());
            return;
        }
        let keep = self.continuous;
        self.submit(false);
        self.continuous = keep;
        if keep {
            self.start_listening();
        }
    }

    /// Send what is typed: replay a learned request or recipe, else ask the on-device model.
    fn submit(&mut self, claude_only: bool) {
        // Sent by hand: the microphone stops (a spoken send turns it back on).
        self.continuous = false;
        if self.listening {
            self.listening = false;
            if let Ok(b) = self.bridge() {
                (b.listen_stop)();
            }
        }
        let typed = std::mem::take(&mut self.input).trim().to_string();
        if let Ok(mut a) = APPLE_REQUEST.lock() {
            *a = None;
        }
        if let Ok(mut g) = GUARD.lock() {
            g.clear();
        }
        if STOP_WORDS.contains(&ai_data::normalise(&typed).as_str()) {
            self.log.push("· Stopped".to_string());
            return;
        }
        self.log.push(format!("You: {typed}"));
        self.busy = true;
        self.last = None;
        self.hints.refresh();
        let request = self.hints.rewrite(&typed);
        let id = ai_data::now_millis();
        if claude_only {
            self.current = Some(Current { id, request: typed, tier: "claude", earlier: String::new(), calls: Vec::new() });
            self.ask_claude(&request);
            return;
        }
        if let Some((tier, calls)) = self.hints.replay(&request) {
            self.current = Some(Current { id, request: typed, tier, earlier: String::new(), calls: Vec::new() });
            self.pending = Some(calls);
            repaint();
            return;
        }
        if self.hints.wants_claude(&request) {
            match self.second_tier() {
                Some("claude") => {
                    self.log.push("· A request with several steps: asking Claude directly".to_string());
                    self.current = Some(Current { id, request: typed, tier: "claude", earlier: String::new(), calls: Vec::new() });
                    self.ask_claude(&request);
                    return;
                }
                Some(_) => {
                    self.log.push("· A request with several steps, and no internet: asking the local model".to_string());
                    self.current = Some(Current { id, request: typed, tier: "local", earlier: String::new(), calls: Vec::new() });
                    self.ask_local(&request);
                    return;
                }
                None => {}
            }
        }
        self.current = Some(Current { id, request: typed, tier: "apple", earlier: String::new(), calls: Vec::new() });
        if let Ok(mut a) = APPLE_REQUEST.lock() {
            *a = Some(ai_data::normalise(&request));
        }
        let prompt = self.hints.prompt(&request);
        if let Ok(b) = self.bridge() {
            // Each request starts clean: the design is the memory, and the model's own is small.
            (b.reset)();
            if let Ok(c) = CString::new(prompt.replace('\0', " ")) {
                (b.ask)(c.as_ptr(), done_cb);
            }
        }
    }

    /// Whether the iPad has a network path.
    fn online(&mut self) -> bool {
        self.bridge().map(|b| (b.online)() == 1).unwrap_or(true)
    }

    /// Which model answers what the on-device one can't: Claude when online with a key, else the local model.
    fn second_tier(&mut self) -> Option<&'static str> {
        if self.has_key && !self.claude_off && self.online() {
            Some("claude")
        } else if self.local_state == "ready" {
            Some("local")
        } else {
            None
        }
    }

    /// Ask the local model (the offline tier): the same tools, running on the iPad.
    fn ask_local(&mut self, request: &str) {
        if let Ok(mut g) = GUARD.lock() {
            g.clear();
        }
        if let Ok(mut a) = APPLE_REQUEST.lock() {
            *a = None;
        }
        let prompt = self.hints.prompt(request);
        if let Ok(b) = self.bridge() {
            if let Ok(p) = CString::new(prompt.replace('\0', " ")) {
                (b.local_ask)(p.as_ptr(), done_cb);
            }
        }
    }

    /// Ask Claude (the second tier) the request, with what an earlier attempt did, if anything.
    fn ask_claude(&mut self, request: &str) {
        // Claude starts with a clean slate: the on-device model's hand-over is not a reason to stop it.
        if let Ok(mut g) = GUARD.lock() {
            g.clear();
        }
        if let Ok(mut a) = APPLE_REQUEST.lock() {
            *a = None;
        }
        let earlier = self.current.as_ref().map(|c| c.earlier.clone()).unwrap_or_default();
        let mut prompt = self.hints.prompt(request);
        if let Some((q, a)) = &self.last_exchange {
            prompt = format!("Previous request: {q}\nPrevious answer: {}\n{prompt}", a.chars().take(400).collect::<String>());
        }
        if !earlier.is_empty() {
            prompt = format!("{prompt}\nAn earlier attempt by the on-device model already applied: {earlier}");
        }
        let model = self.hints.claude_model.clone().unwrap_or_else(|| CLAUDE_MODEL.to_string());
        if let Ok(b) = self.bridge() {
            if let (Ok(p), Ok(m)) = (CString::new(prompt.replace('\0', " ")), CString::new(model.replace('\0', " "))) {
                (b.claude_ask)(p.as_ptr(), m.as_ptr(), done_cb);
            }
        }
    }

    /// The clipboard's text (iOS asks to allow pasting the first time).
    fn clipboard(&mut self) -> String {
        let Ok(b) = self.bridge() else { return String::new() };
        let p = (b.paste)();
        if p.is_null() {
            return String::new();
        }
        // SAFETY: the framework returns a `strdup`ed C string.
        let text = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        unsafe { libc::free(p.cast()) };
        text
    }

    /// Whether a Claude key is saved, and the usage so far.
    fn refresh_key(&mut self) {
        let (has, usage) = match self.bridge() {
            Ok(b) => {
                let has = (b.has_key)() == 1;
                let p = (b.usage)();
                let text = if p.is_null() {
                    String::new()
                } else {
                    // SAFETY: the framework returns a `strdup`ed C string.
                    let t = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
                    unsafe { libc::free(p.cast()) };
                    t
                };
                (has, text)
            }
            Err(_) => (false, String::new()),
        };
        self.has_key = has;
        if let Ok(b) = self.bridge() {
            let p = (b.local_state)();
            if !p.is_null() {
                // SAFETY: the framework returns a `strdup`ed C string.
                self.local_state = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
                unsafe { libc::free(p.cast()) };
            }
        }
        self.usage = serde_json::from_str::<Value>(&usage)
            .map(|u| format!("Claude so far: {} requests, {} tokens in, {} out", u["requests"], u["input"], u["output"]))
            .unwrap_or_default();
    }

    /// Write the finished request to the log and offer to rate it.
    fn finish(&mut self, answer: String) {
        let Some(cur) = self.current.take() else {
            self.busy = false;
            self.log.push(format!("AI: {answer}"));
            return;
        };
        let error = answer.starts_with("error");
        let clean = !error
            && !cur.calls.is_empty()
            && cur.calls.iter().all(|(_, r)| !(r.starts_with("error") || r.starts_with("refused") || r.starts_with("no ")));
        // The on-device model could not do it: hand over to Claude, telling it what was already done.
        let second = if cur.tier == "apple" && !clean { self.second_tier() } else { None };
        let escalate = second.is_some();
        ai_data::log(&json!({
            "kind": "request", "id": cur.id, "t": cur.id / 1000, "request": cur.request, "tier": cur.tier,
            "calls": cur.calls.iter().map(|(c, r)| json!({ "call": c, "reply": r })).collect::<Vec<_>>(),
            "answer": answer, "error": error, "escalated": escalate,
        }));
        if escalate {
            let done: Vec<String> = cur
                .calls
                .iter()
                .filter(|(c, r)| r.starts_with("ok") && c.get("tool").and_then(Value::as_str).is_some_and(|t| t != "find" && t != "inspect"))
                .filter_map(|(c, r)| Some(format!("{} -> {r}", ai_data::describe_call(c)?)))
                .collect();
            let id = ai_data::now_millis();
            let request = self.hints.rewrite(&cur.request);
            if second == Some("claude") {
                self.log.push("· The on-device model couldn't do that: asking Claude…".to_string());
                self.current = Some(Current { id, request: cur.request.clone(), tier: "claude", earlier: done.join("; "), calls: Vec::new() });
                self.ask_claude(&request);
            } else {
                self.log.push("· The on-device model couldn't do that, and there is no internet: asking the local model…".to_string());
                self.current = Some(Current { id, request: cur.request.clone(), tier: "local", earlier: done.join("; "), calls: Vec::new() });
                self.ask_local(&request);
            }
            return;
        }
        self.busy = false;
        self.last_exchange = Some((cur.request.clone(), answer.clone()));
        let shown = if answer == "error: offline" { "error: no internet, so Claude could not be asked".to_string() } else { answer };
        self.log.push(format!("AI: {shown}"));
        self.last = Some(Rated { id: cur.id, request: cur.request, tier: cur.tier, calls: cur.calls.into_iter().map(|(c, _)| c).collect(), clean });
    }

    /// Good, Partly or Poor for the last request: logged, and a Good one from the model is remembered.
    fn rate(&mut self, value: &str) {
        let Some(r) = self.last.take() else { return };
        ai_data::log(&json!({ "kind": "feedback", "id": r.id, "t": ai_data::now_millis() / 1000, "value": value }));
        match value {
            "good" => {
                if matches!(r.tier, "apple" | "claude" | "local") && r.clean {
                    // Only the calls that changed something: looking things up is not part of a recipe.
                    let changing: Vec<Value> = r.calls.iter().filter(|c| !matches!(c.get("tool").and_then(Value::as_str), Some("find" | "inspect"))).cloned().collect();
                    self.hints.learn(&r.request, &changing);
                    self.log.push("· Good: remembered, the same request now runs at once".to_string());
                } else {
                    self.log.push("· Good".to_string());
                }
            }
            "partial" => self.log.push("· Partly: logged for review, not remembered".to_string()),
            _ => {
                if r.tier == "learned" {
                    self.hints.forget(&r.request);
                }
                self.log.push("· Poor: logged for review".to_string());
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
                    // Nothing heard for a while is not a reason to stop.
                    if self.continuous && (text.contains("No speech") || text.contains("1110")) {
                        self.start_listening();
                    } else {
                        self.continuous = false;
                        self.log.push(text);
                    }
                }
                // Words that arrive after Stop or after the request was sent are dropped: they would overwrite what is there.
                _ if !self.listening => {}
                _ => {
                    self.input = format!("{}{}", self.speech_base, text);
                    self.heard_at = ai_data::now_millis();
                    if kind == 1 {
                        self.listening = false;
                        if !self.no_auto_send && !self.busy && !self.input.trim().is_empty() {
                            self.submit_spoken();
                        } else if self.continuous {
                            self.start_listening();
                        }
                    }
                }
            }
        }
        for (text, kind) in LOCAL.lock().map(|mut s| std::mem::take(&mut *s)).unwrap_or_default() {
            match kind {
                0 => self.local_progress = text,
                1 => {
                    self.local_state = "ready".to_string();
                    self.local_progress.clear();
                    self.log.push("· The local model is ready for offline use".to_string());
                }
                _ => {
                    self.local_state = "none".to_string();
                    self.local_progress.clear();
                    self.log.push(text);
                }
            }
        }
        let replayed = self.pending.take();
        if let Some(calls) = &replayed {
            // A saved request stops at the first call that fails or is refused: the rest would
            // build on a design that is not what it was recorded on.
            for call in calls {
                let reply = handle(app, &call.to_string());
                if reply.starts_with("error") || reply.starts_with("refused") {
                    break;
                }
            }
        }
        if let Some(cur) = self.current.as_mut() {
            cur.calls.extend(CALLS.lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default());
        } else if let Ok(mut c) = CALLS.lock() {
            c.clear();
        }
        if replayed.is_some() {
            let said = self.current.as_ref().map(|c| c.calls.iter().map(|(_, r)| r.as_str()).collect::<Vec<_>>().join("; ")).unwrap_or_default();
            self.finish(format!("(saved request) {said}"));
        }
        let answers: Vec<_> = ANSWERS.lock().map(|mut a| std::mem::take(&mut *a)).unwrap_or_default();
        for a in answers {
            self.finish(a);
        }
    }

    /// Whether the window is showing.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// `lift`: how far a docked keyboard reaches up from the bottom of the screen (0: none).
    pub fn show(&mut self, ctx: &egui::Context, lift: f32) {
        let _ = CTX.set(ctx.clone());
        if self.listening {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            // A pause after some words sends the request.
            if !self.no_auto_send && !self.busy && !self.input.trim().is_empty() && ai_data::now_millis().saturating_sub(self.heard_at) > 1500 {
                self.submit_spoken();
            }
        }
        let pulse = if self.listening { 0.55 + 0.45 * (ctx.input(|i| i.time) * 5.0).sin().abs() as f32 } else { 0.0 };
        egui::Area::new(egui::Id::new("ai_button")).anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -96.0]).show(ctx, |ui| {
            let mut ai_button = egui::Button::new("AI");
            if self.listening {
                ai_button = ai_button.fill(egui::Color32::from_rgb((220.0 * pulse) as u8, 30, 30));
            }
            if ui.add_sized([56.0, 44.0], ai_button).clicked() {
                self.open = !self.open;
                // Makes the Documents/ai folder (log, learned requests, hints) show in Files.
                self.hints.refresh();
                self.refresh_key();
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
            if self.listening {
                ui.label(egui::RichText::new("● Listening… say a command; tap Stop to end").strong().color(egui::Color32::from_rgb((255.0 * pulse) as u8, 60, 60)));
            } else {
                ui.label(egui::RichText::new(status).weak());
            }
            let mut auto = !self.no_auto_send;
            if ui.checkbox(&mut auto, "Send after a pause when speaking").changed() {
                self.no_auto_send = !auto;
            }
            egui::ScrollArea::vertical().max_height(260.0).stick_to_bottom(true).show(ui, |ui| {
                for line in &self.log {
                    ui.label(line);
                }
                if self.busy {
                    ui.label("…thinking");
                }
            });
            if self.last.is_some() {
                ui.horizontal(|ui| {
                    ui.label("Was that right?");
                    if ui.button("Good").clicked() {
                        self.rate("good");
                    }
                    if ui.button("Partly").clicked() {
                        self.rate("partial");
                    }
                    if ui.button("Poor").clicked() {
                        self.rate("poor");
                    }
                });
            }
            ui.collapsing("Local model (offline)", |ui| {
                ui.label("Used when there is no internet. It downloads once (about 2.3 GB, over Wi-Fi) and then works offline.");
                match self.local_state.as_str() {
                    "ready" => {
                        ui.label("Ready.");
                    }
                    "loading" => {
                        ui.label(format!("Downloading… {}", self.local_progress));
                    }
                    _ => {
                        if ui.button("Download the local model").clicked() {
                            self.local_state = "loading".to_string();
                            self.local_progress = "0%".to_string();
                            if let Ok(b) = self.bridge() {
                                (b.local_load)(local_cb);
                            }
                        }
                    }
                }
            });
            ui.collapsing("Claude (second tier)", |ui| {
                if self.has_key {
                    ui.label("Key saved in the Keychain. Claude is asked only when the on-device model can't do a request, or with Ask Claude.");
                    ui.checkbox(&mut self.claude_off, "Do not ask Claude automatically");
                    ui.label(egui::RichText::new(self.usage.clone()).weak());
                    if ui.button("Remove key").clicked() {
                        if let Ok(b) = self.bridge() {
                            (b.clear_key)();
                        }
                        self.refresh_key();
                    }
                } else {
                    ui.label("Paste an Anthropic API key to let Claude help with requests the on-device model can't do. Requests then leave the device when that happens.");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.key_input).password(true).desired_width(ui.available_width() - 90.0).hint_text("sk-ant-…"));
                        if ui.button("Paste").clicked() {
                            let text = self.clipboard();
                            self.key_input = text.trim().to_string();
                        }
                    });
                    if !self.key_input.is_empty() {
                        ui.label(egui::RichText::new(format!("{} characters entered", self.key_input.chars().count())).weak());
                    }
                    if ui.add_enabled(!self.key_input.trim().is_empty(), egui::Button::new("Save key")).clicked() {
                        let key = std::mem::take(&mut self.key_input);
                        if let (Ok(b), Ok(c)) = (self.bridge(), CString::new(key.trim())) {
                            let saved = (b.set_key)(c.as_ptr()) == 1;
                            self.log.push(if saved { "· Claude key saved".to_string() } else { "· The key could not be saved".to_string() });
                        }
                        self.refresh_key();
                    }
                }
            });
            ui.separator();
            // Enter asks; Shift+Enter is a new line.
            let id = egui::Id::new("ai_input");
            let enter = ui.memory(|m| m.has_focus(id)) && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            let edit = ui.add(egui::TextEdit::multiline(&mut self.input).id(id).desired_rows(2).desired_width(f32::INFINITY).hint_text("Ask SolidCraft to do something"));
            if edit.changed() {
                self.stop_speaking();
            }
            ui.horizontal_wrapped(|ui| {
                let ready = !self.busy && !self.input.trim().is_empty() && self.availability == "available";
                if (ui.add_enabled(ready, egui::Button::new("Ask")).clicked() || (enter && ready)) && ready {
                    self.submit(false);
                }
                if self.has_key && ui.add_enabled(!self.busy && !self.input.trim().is_empty(), egui::Button::new("Ask Claude")).clicked() {
                    self.submit(true);
                }
                if self.local_state == "ready" && ui.add_enabled(!self.busy && !self.input.trim().is_empty(), egui::Button::new("Ask local")).clicked() {
                    let typed = self.input.trim().to_string();
                    self.input.clear();
                    self.log.push(format!("You: {typed}"));
                    self.busy = true;
                    self.last = None;
                    let id = ai_data::now_millis();
                    self.current = Some(Current { id, request: typed.clone(), tier: "local", earlier: String::new(), calls: Vec::new() });
                    self.ask_local(&self.hints.rewrite(&typed));
                }
                if ui.button("Paste").clicked() {
                    self.stop_speaking();
                    let text = self.clipboard();
                    if !text.is_empty() {
                        if !self.input.is_empty() && !self.input.ends_with(' ') {
                            self.input.push(' ');
                        }
                        self.input.push_str(text.trim());
                    }
                }
                let speak = if self.listening { egui::Button::new("Stop").fill(egui::Color32::from_rgb(190, 40, 40)) } else { egui::Button::new("Speak") };
                if ui.add(speak).clicked() {
                    if self.listening || self.continuous {
                        self.stop_speaking();
                    } else {
                        self.continuous = !self.no_auto_send;
                        self.start_listening();
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
