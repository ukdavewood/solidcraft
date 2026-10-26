//! What the AI window keeps in the app's `Documents/ai` folder (visible in the Files app):
//!
//! * `ai-log.jsonl`: one JSON line per request (what was asked, which tier answered, the tool
//!   calls and their replies, the answer) and one per Good/Bad rating.
//! * `ai-learned.json`: requests rated Good, with the calls that worked; the same request again
//!   replays them without asking a model. A Bad rating on a replay removes the entry.
//! * `ai-hints.json`: written by hand or by the Claude session on the Mac, read here on every
//!   request, so improving the AI needs no rebuild:
//!   `{"rewrites":[{"from":"clear the model","to":"start a new design"}],
//!     "instructions":["short extra rule"],
//!     "examples":[{"request":"fillet all edges 5 mm","call":"fillet_edges(size=5, which=all)"}],
//!     "recipes":[{"request":"fillet all edges {n} mm","calls":[{"tool":"round","kind":"fillet","size":"{n}","which":"all","body":""}]}]}`
//!   A recipe's `{n}` (or `{n2}`…) stands for a number in the request and is put into the calls.
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// The log is moved aside when it passes this size.
const MAX_LOG_BYTES: u64 = 2_000_000;
/// The most learned requests kept.
const MAX_LEARNED: usize = 200;
/// The most text hints add to a prompt: the on-device model's memory is small.
const MAX_HINT_CHARS: usize = 900;

pub fn dir() -> PathBuf {
    let d = crate::files::documents().join("ai");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Add one JSON line to the log.
pub fn log(entry: &Value) {
    let path = dir().join("ai-log.jsonl");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::rename(&path, dir().join("ai-log.old.jsonl"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{entry}");
    }
}

/// A request as words only, so "Fillet all edges 5mm." and "fillet all edges 5 mm" match.
pub fn normalise(text: &str) -> String {
    let spaced: String = text.chars().map(|c| if c.is_alphanumeric() || c == '.' { c.to_ascii_lowercase() } else { ' ' }).collect();
    // "5mm" is "5" and "mm"; the unit words and fillers are dropped.
    let mut words: Vec<String> = Vec::new();
    for w in spaced.split_whitespace() {
        let w = w.trim_matches('.');
        let (num, unit) = match w.find(|c: char| c.is_alphabetic()) {
            Some(i) if i > 0 && w[..i].chars().all(|c| c.is_ascii_digit() || c == '.') => (&w[..i], &w[i..]),
            _ => (w, ""),
        };
        for part in [num, unit] {
            if !part.is_empty() && !["mm", "millimetre", "millimetres", "millimeter", "millimeters", "please", "the"].contains(&part) {
                words.push(part.to_string());
            }
        }
    }
    words.join(" ")
}

fn read_json(name: &str) -> Option<(Value, SystemTime)> {
    let path = dir().join(name);
    let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
    let text = std::fs::read_to_string(&path).ok()?;
    Some((serde_json::from_str(&text).ok()?, modified))
}

struct Recipe {
    template: Vec<String>,
    calls: Vec<Value>,
}

/// A call as the model's tool call reads, for the examples it is shown.
pub fn describe_call(call: &Value) -> Option<String> {
    let text = |k: &str| call.get(k).and_then(Value::as_str).unwrap_or("");
    let num = |k: &str| call.get(k).and_then(Value::as_f64).map(|n| format!("{n}")).unwrap_or_default();
    match text("tool") {
        "round" => Some(format!("{}_edges(size={}, which={}, body=\"\")", if text("kind") == "chamfer" { "chamfer" } else { "fillet" }, num("size"), text("which"))),
        "shape" => match text("kind") {
            "cube" => Some(format!("add_cube(size={})", num("a"))),
            "box" => Some(format!("add_box(length={}, width={}, height={})", num("a"), num("b"), num("c"))),
            "cylinder" => Some(format!("add_cylinder(radius={}, height={})", num("a"), num("b"))),
            "sphere" => Some(format!("add_sphere(radius={})", num("a"))),
            _ => None,
        },
        "inspect" => Some("inspect_design()".to_string()),
        "run" => match text("command") {
            "view.show_hide" => Some(format!("show_hide(rule=\"{}\")", call.get("params").and_then(|p| p.get("rule")).and_then(Value::as_str)?)),
            "parameters.change" => {
                let p = call.get("params")?;
                Some(format!("set_parameter(name={}, value={})", p.get("name")?.as_str()?, p.get("expression")?.as_str()?))
            }
            "edit.undo" => Some("undo()".to_string()),
            "file.new" => Some("new_design()".to_string()),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Default)]
pub struct Hints {
    rewrites: Vec<(String, String)>,
    instructions: Vec<String>,
    examples: Vec<(String, String)>,
    recipes: Vec<Recipe>,
    /// Requests rated Good: (normalised request, original words, calls).
    learned: Vec<(String, String, Vec<Value>)>,
    /// The Claude model to ask (`claude_model` in the hints file), else the built-in one.
    pub claude_model: Option<String>,
    hints_stamp: Option<SystemTime>,
    learned_stamp: Option<SystemTime>,
}

impl Hints {
    /// Read the files again when they changed (cheap: two file dates).
    pub fn refresh(&mut self) {
        let path = dir().join("ai-hints.json");
        if !path.exists() {
            let _ = std::fs::write(&path, "{\"rewrites\":[],\"instructions\":[],\"examples\":[],\"recipes\":[]}\n");
        }
        let stamp = |n: &str| std::fs::metadata(dir().join(n)).ok().and_then(|m| m.modified().ok());
        if stamp("ai-hints.json") != self.hints_stamp {
            self.hints_stamp = stamp("ai-hints.json");
            self.load_hints();
        }
        if stamp("ai-learned.json") != self.learned_stamp {
            self.learned_stamp = stamp("ai-learned.json");
            self.load_learned();
        }
    }

    fn load_hints(&mut self) {
        let Some((v, _)) = read_json("ai-hints.json") else { return };
        let strings = |a: &Value, k1: &str, k2: &str| -> Vec<(String, String)> {
            a.as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| Some((e.get(k1)?.as_str()?.to_string(), e.get(k2)?.as_str()?.to_string())))
                .collect()
        };
        self.claude_model = v.get("claude_model").and_then(Value::as_str).map(str::to_string);
        self.rewrites = strings(v.get("rewrites").unwrap_or(&Value::Null), "from", "to").into_iter().map(|(a, b)| (a.to_lowercase(), b)).collect();
        self.instructions = v.get("instructions").and_then(Value::as_array).into_iter().flatten().filter_map(|s| s.as_str().map(str::to_string)).collect();
        self.examples = strings(v.get("examples").unwrap_or(&Value::Null), "request", "call");
        self.recipes = v
            .get("recipes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|r| {
                let template = normalise_keep_slots(r.get("request")?.as_str()?);
                let calls = r.get("calls")?.as_array()?.clone();
                Some(Recipe { template, calls })
            })
            .collect();
    }

    fn load_learned(&mut self) {
        let Some((v, _)) = read_json("ai-learned.json") else { return };
        self.learned = v
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let request = e.get("request")?.as_str()?.to_string();
                Some((normalise(&request), request, e.get("calls")?.as_array()?.clone()))
            })
            .collect();
    }

    /// The request with the hints' rewrites applied (whole phrases, any case).
    pub fn rewrite(&self, request: &str) -> String {
        let mut out = request.to_string();
        for (from, to) in &self.rewrites {
            let lower = out.to_lowercase();
            if let Some(i) = lower.find(from.as_str()) {
                // Lower-casing keeps byte positions for ASCII; anything else is left alone.
                if lower.len() == out.len() && out.is_char_boundary(i) && out.is_char_boundary(i + from.len()) {
                    out.replace_range(i..i + from.len(), to);
                }
            }
        }
        out
    }

    /// Calls to run instead of asking a model: a learned request, else a matching recipe.
    pub fn replay(&self, request: &str) -> Option<(&'static str, Vec<Value>)> {
        let norm = normalise(request);
        if let Some((_, _, calls)) = self.learned.iter().find(|(n, _, _)| *n == norm) {
            return Some(("learned", calls.clone()));
        }
        let words: Vec<&str> = norm.split(' ').collect();
        for r in &self.recipes {
            if r.template.len() != words.len() {
                continue;
            }
            let mut slots: BTreeMap<String, String> = BTreeMap::new();
            let ok = r.template.iter().zip(&words).all(|(t, w)| {
                if t.starts_with('{') && t.ends_with('}') {
                    w.parse::<f64>().is_ok() && slots.insert(t.clone(), (*w).to_string()).is_none()
                } else {
                    t == w
                }
            });
            if ok {
                return Some(("recipe", r.calls.iter().map(|c| fill(c, &slots)).collect()));
            }
        }
        None
    }

    /// The text put in front of a request for the on-device model: the extra rules and the
    /// few examples that share the most words with it, within [`MAX_HINT_CHARS`].
    pub fn prompt(&self, request: &str) -> String {
        let words: Vec<String> = normalise(request).split(' ').map(str::to_string).collect();
        let overlap = |text: &str| normalise(text).split(' ').filter(|w| words.iter().any(|x| x == w)).count();
        let mut pool: Vec<(usize, String)> = self.examples.iter().map(|(r, c)| (overlap(r), format!("- \"{r}\" -> {c}"))).collect();
        for (_, original, calls) in &self.learned {
            let described: Vec<String> = calls.iter().filter_map(describe_call).collect();
            if !described.is_empty() {
                pool.push((overlap(original), format!("- \"{original}\" -> {}", described.join("; "))));
            }
        }
        pool.retain(|(score, _)| *score > 0);
        pool.sort_by(|a, b| b.0.cmp(&a.0));
        let mut text = String::new();
        for line in self.instructions.iter().take(5) {
            text.push_str(line);
            text.push('\n');
        }
        if !pool.is_empty() {
            text.push_str("Examples of requests and the tool calls that worked:\n");
            for (_, line) in pool.iter().take(3) {
                text.push_str(line);
                text.push('\n');
            }
        }
        let mut end = text.len().min(MAX_HINT_CHARS);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        if text.is_empty() { format!("Request: {request}") } else { format!("{text}\nRequest: {request}") }
    }

    /// Remember a Good request and its calls (the same words again replay them).
    pub fn learn(&mut self, request: &str, calls: &[Value]) {
        let norm = normalise(request);
        if norm.is_empty() || calls.is_empty() {
            return;
        }
        self.learned.retain(|(n, _, _)| *n != norm);
        self.learned.push((norm, request.to_string(), calls.to_vec()));
        if self.learned.len() > MAX_LEARNED {
            self.learned.remove(0);
        }
        self.save_learned();
    }

    /// Forget a learned request (rated Bad after a replay).
    pub fn forget(&mut self, request: &str) {
        let norm = normalise(request);
        self.learned.retain(|(n, _, _)| *n != norm);
        self.save_learned();
    }

    fn save_learned(&mut self) {
        let list: Vec<Value> = self.learned.iter().map(|(_, r, c)| json!({ "request": r, "calls": c })).collect();
        let _ = std::fs::write(dir().join("ai-learned.json"), serde_json::to_string_pretty(&list).unwrap_or_default());
        self.learned_stamp = std::fs::metadata(dir().join("ai-learned.json")).ok().and_then(|m| m.modified().ok());
    }
}

/// A recipe's request as words, its `{n}` slots kept.
fn normalise_keep_slots(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for w in text.split_whitespace() {
        if w.starts_with('{') && w.ends_with('}') {
            out.push(w.to_string());
        } else {
            let n = normalise(w);
            out.extend(n.split_whitespace().map(str::to_string));
        }
    }
    out
}

/// A call with the `{n}` slots filled in.
fn fill(value: &Value, slots: &BTreeMap<String, String>) -> Value {
    match value {
        Value::String(s) => match slots.get(s) {
            Some(n) => n.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or_else(|| Value::String(n.clone())),
            None => {
                let mut t = s.clone();
                for (k, v) in slots {
                    t = t.replace(k.as_str(), v);
                }
                Value::String(t)
            }
        },
        Value::Array(a) => Value::Array(a.iter().map(|x| fill(x, slots)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), fill(x, slots))).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_normalise_to_words() {
        assert_eq!(normalise("Fillet ALL edges 5mm."), "fillet all edges 5");
        assert_eq!(normalise("fillet all edges 5 millimetres, please"), "fillet all edges 5");
        assert_eq!(normalise("Add a 2.5 mm cube"), "add a 2.5 cube");
    }

    #[test]
    fn recipes_fill_numbers_and_learned_requests_replay() {
        let mut h = Hints::default();
        h.recipes.push(Recipe {
            template: normalise_keep_slots("fillet all edges {n} mm"),
            calls: vec![json!({"tool": "round", "kind": "fillet", "size": "{n}", "which": "all", "body": ""})],
        });
        let (kind, calls) = h.replay("Fillet all edges 3.5mm").unwrap();
        assert_eq!(kind, "recipe");
        assert_eq!(calls[0]["size"], json!(3.5));
        assert!(h.replay("fillet all edges").is_none());
        assert!(h.replay("fillet some edges 5").is_none());
        h.learned.push((normalise("hide everything"), "hide everything".into(), vec![json!({"tool": "run", "command": "view.show_hide", "params": {"rule": "-"}})]));
        assert_eq!(h.replay("Hide everything!").unwrap().0, "learned");
    }

    #[test]
    fn prompts_carry_the_closest_examples_and_calls_read_back() {
        let mut h = Hints::default();
        h.examples.push(("add a cube".into(), "add_shape(kind=box, size1=10, size2=10, size3=10)".into()));
        h.examples.push(("hide the lamps".into(), "show_hide(rule=\"-lamp\")".into()));
        h.instructions.push("Lengths are millimetres.".into());
        let p = h.prompt("hide all the lamps please");
        assert!(p.contains("hide the lamps") && !p.contains("add a cube") && p.ends_with("Request: hide all the lamps please"));
        let call = json!({"tool": "round", "kind": "chamfer", "size": 2.0, "which": "top", "body": ""});
        assert_eq!(describe_call(&call).unwrap(), "chamfer_edges(size=2, which=top, body=\"\")");
        h.rewrites.push(("clear the model".into(), "start a new design".into()));
        assert_eq!(h.rewrite("Please CLEAR THE MODEL now"), "Please start a new design now");
    }
}
