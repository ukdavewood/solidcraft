//! `view.show_hide`: show and hide bodies from a rule string, so a model with many variants,
//! decks or stages is switched between views with one short command.
//!
//! A rule string is `;`-separated steps of `,`-separated terms, applied left to right, each to
//! every body in the state the terms before it left:
//!
//! | Term | Effect |
//! |---|---|
//! | `+` / `-` | show / hide every body |
//! | `+text` / `-text` | show / hide the bodies whose `component:group:body:appearance` text contains `text` (any case) |
//! | `+@hull` / `-@hull` | show / hide the bodies that share volume with a body whose name contains `hull` (the hulls themselves are left alone) |
//! | `+!@hull` / `-!@hull` | the same for the bodies that do **not** share volume with a hull |
//! | `U` | show every placement again (hidden occurrences) |
//! | `{name=value}` | set a user parameter (a number or expression) at that point |
//! | `_name` | a shortcut: a named rule kept in the design's metadata under `io.solidcraft.show_hide` (`{"_S5": "-,+deck5"}`) |
//!
//! A shortcut expands only as a whole term (between `,` or `;`, spaces and case ignored), so a
//! body name that merely contains `_S5` is never mistaken for the shortcut `_S5`. Shortcuts may
//! hold other shortcuts (12 levels deep; a loop is an error). Text inside `{...}` is never
//! expanded. Camera and viewport terms (`V1`, `V:name`, `C`, `P`, `L0`, `L1`, `SHOT…`,
//! `ORBIT…`) and tags (`[+tag]`) belong to the host, not the design: they are skipped and listed
//! in `ignored`.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::CommandSpec;
use crate::params::{bad, str_};
use crate::{Result, Session};

pub static COMMANDS: &[CommandSpec] = &[CommandSpec::new("view.show_hide", "Show/Hide by Rule", show_hide).icon("eye").noundo().params(
    "rule: text (steps separated by ;, terms by ,: `-` hides all, `+x` / `-x` shows / hides the bodies whose component:group:body:appearance contains x, \
     `+@hull` / `-@hull` / `+!@hull` / `-!@hull` by overlap with the bodies named hull, `U` shows hidden placements, `{param=value}` sets a user parameter, \
     `_name` runs the shortcut of that name kept in the design's metadata under io.solidcraft.show_hide); dry_run?: bool (report only). → shown, hidden, visible, terms, ignored: [terms for the host]. \
     Saved with the design; not an undo step",
)];

/// How deep shortcuts may nest.
const MAX_DEPTH: usize = 12;
/// The most terms one rule may expand to.
const MAX_TERMS: usize = 10_000;
/// The longest rule text accepted.
const MAX_RULE: usize = 100_000;

#[derive(Clone, Debug, PartialEq)]
enum Op {
    /// `+` or `-` alone.
    All { show: bool },
    /// `+text` or `-text`: the lower-case text.
    Name { show: bool, text: String },
    /// `+@h`, `-@h`, `+!@h`, `-!@h`: `outside` for `!@`.
    Overlap { show: bool, outside: bool, hull: String },
    /// `U`.
    Unhide,
    /// `{name=value}`: one assignment.
    Assign { name: String, value: String },
    /// A term for the host (camera, tags, …) or one that is not a term.
    Ignored(String),
}

/// Split `text` at top-level `,` and `;`, keeping `{…}` and `[…]` whole.
fn split_terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '{' | '[' => {
                depth += 1;
                cur.push(c);
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            ',' | ';' if depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect()
}

/// The shortcut name of a whole term: lower case, spaces removed.
fn shortcut_key(term: &str) -> String {
    term.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}

/// Expand the shortcuts that are whole terms, in order, nested up to [`MAX_DEPTH`].
fn flatten(text: &str, table: &[(String, String)], depth: usize, out: &mut Vec<String>) -> std::result::Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("shortcuts nest too deeply (a shortcut contains itself?)".to_string());
    }
    for term in split_terms(text) {
        if out.len() > MAX_TERMS {
            return Err(format!("the rule expands to more than {MAX_TERMS} terms"));
        }
        let key = shortcut_key(&term);
        match key.starts_with('_').then(|| table.iter().find(|(name, _)| *name == key)).flatten() {
            Some((_, value)) => flatten(value, table, depth + 1, out)?,
            None => out.push(term),
        }
    }
    Ok(())
}

fn parse_term(term: &str) -> Vec<Op> {
    if let Some(inner) = term.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
        return inner
            .split(',')
            .filter(|a| !a.trim().is_empty())
            .map(|a| match a.split_once('=') {
                Some((n, v)) if !n.trim().is_empty() => Op::Assign { name: n.trim().to_string(), value: v.trim().to_string() },
                _ => Op::Ignored(format!("{{{a}}}")),
            })
            .collect();
    }
    let upper = term.to_uppercase();
    if upper == "U" {
        return vec![Op::Unhide];
    }
    let host = ["C", "P", "L0", "L1"].contains(&upper.as_str())
        || upper.starts_with("SHOT")
        || upper.starts_with("ORBIT")
        || upper.starts_with("V:")
        || upper.starts_with("VIEW:")
        || (upper.starts_with('V') && upper.len() == 2 && upper[1..].chars().all(|c| c.is_ascii_digit()))
        || (upper.starts_with("VIEW") && upper[4..].chars().all(|c| c.is_ascii_digit()) && upper.len() > 4)
        || term.starts_with('[');
    if host {
        return vec![Op::Ignored(term.to_string())];
    }
    let (show, rest) = match term.chars().next() {
        Some('+') => (true, &term[1..]),
        Some('-') => (false, &term[1..]),
        Some('@') | Some('!') => (true, term),
        _ => return vec![Op::Ignored(term.to_string())],
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return vec![Op::All { show }];
    }
    let (outside, after) = match rest.strip_prefix("!@") {
        Some(a) => (true, Some(a)),
        None => (false, rest.strip_prefix('@')),
    };
    match after {
        Some(hull) if !hull.trim().is_empty() => vec![Op::Overlap { show, outside, hull: hull.trim().to_lowercase() }],
        Some(_) => vec![Op::Ignored(term.to_string())],
        None => vec![Op::Name { show, text: rest.to_lowercase() }],
    }
}

/// The design's metadata key the shortcuts live under: an object of `"_name": "rule text"`.
pub const SHORTCUTS_KEY: &str = "io.solidcraft.show_hide";

/// The design's shortcuts: (lower-case name starting with `_`, rule text).
fn shortcut_table(s: &Session) -> Vec<(String, String)> {
    s.doc
        .metadata
        .get(SHORTCUTS_KEY)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(name, _)| name.starts_with('_'))
        .filter_map(|(name, value)| Some((shortcut_key(name), value.as_str()?.to_string())))
        .collect()
}

/// A body as the rules see it: its (world) name and the text name terms search.
struct Candidate {
    name: String,
    key: String,
}

fn candidates(s: &Session) -> Vec<Candidate> {
    let st = s.world_state();
    st.bodies
        .iter()
        .map(|b| {
            let component = s.doc.model_body_component(b);
            let component_name = s.doc.components.iter().find(|c| c.id == component).map(|c| c.name.as_str()).unwrap_or("");
            let definition = b.definition_name();
            let group = s
                .doc
                .browser_groups
                .iter()
                .find(|g| g.folder == "bodies" && g.component == component && g.items.iter().any(|i| i == definition))
                .map(|g| g.name.as_str())
                .unwrap_or("");
            let appearance = s.doc.model_body_look(b).map(|l| l.name).unwrap_or_default();
            Candidate { name: b.name.clone(), key: format!("{component_name}:{group}:{}:{appearance}", b.name).to_lowercase() }
        })
        .collect()
}

/// The bodies that share volume with any body whose name contains `hull` (the hulls excluded).
fn overlapping(s: &mut Session, cands: &[Candidate], hull: &str) -> Result<BTreeSet<String>> {
    let hulls: BTreeSet<&str> = cands.iter().filter(|c| c.name.to_lowercase().contains(hull)).map(|c| c.name.as_str()).collect();
    if hulls.is_empty() {
        return Err(bad("view.show_hide", format!("no body has a name containing `{hull}` (the hull of `@{hull}`)")));
    }
    let names: Vec<&str> = cands.iter().map(|c| c.name.as_str()).collect();
    let r = super::interference::interference(s, &json!({ "bodies": names }))?;
    let mut set = BTreeSet::new();
    for pair in r.get("interferences").and_then(Value::as_array).into_iter().flatten() {
        let (Some(a), Some(b)) = (pair.get("a").and_then(Value::as_str), pair.get("b").and_then(Value::as_str)) else { continue };
        match (hulls.contains(a), hulls.contains(b)) {
            (true, false) => {
                set.insert(b.to_string());
            }
            (false, true) => {
                set.insert(a.to_string());
            }
            _ => {}
        }
    }
    // The hulls themselves are never touched by an overlap term.
    Ok(set)
}

fn show_hide(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "view.show_hide";
    let rule = str_(p, "rule").ok_or_else(|| bad(cmd, "`rule` is required (text)"))?;
    if rule.len() > MAX_RULE {
        return Err(bad(cmd, format!("`rule` is longer than {MAX_RULE} characters")));
    }
    let dry = p.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let table = shortcut_table(s);
    let mut terms = Vec::new();
    flatten(rule, &table, 0, &mut terms).map_err(|e| bad(cmd, e))?;
    let ops: Vec<Op> = terms.iter().flat_map(|t| parse_term(t)).collect();

    let mut cands = candidates(s);
    let mut hidden: BTreeSet<String> = s.visibility.hidden_bodies.iter().cloned().collect();
    let mut unhide = false;
    let mut ignored = Vec::new();
    let mut shown_by: BTreeSet<String> = BTreeSet::new();
    for op in &ops {
        match op {
            Op::All { show } => {
                for c in &cands {
                    if *show {
                        hidden.remove(&c.name);
                        shown_by.insert(c.name.clone());
                    } else {
                        hidden.insert(c.name.clone());
                    }
                }
            }
            Op::Name { show, text } => {
                for c in cands.iter().filter(|c| c.key.contains(text.as_str())) {
                    if *show {
                        hidden.remove(&c.name);
                        shown_by.insert(c.name.clone());
                    } else {
                        hidden.insert(c.name.clone());
                    }
                }
            }
            Op::Overlap { show, outside, hull } => {
                let touching = overlapping(s, &cands, hull)?;
                for c in cands.iter().filter(|c| !c.name.to_lowercase().contains(hull.as_str())) {
                    if touching.contains(&c.name) != *outside {
                        if *show {
                            hidden.remove(&c.name);
                            shown_by.insert(c.name.clone());
                        } else {
                            hidden.insert(c.name.clone());
                            }
                    }
                }
            }
            Op::Unhide => unhide = true,
            Op::Assign { name, value } => {
                if dry {
                    ignored.push(format!("{{{name}={value}}} (dry run: not set)"));
                    continue;
                }
                s.execute("parameters.change", &json!({ "name": name, "expression": value }))?;
                // The bodies may have changed with the parameter.
                cands = candidates(s);
            }
            Op::Ignored(t) => ignored.push(t.clone()),
        }
    }
    let names: BTreeSet<&str> = cands.iter().map(|c| c.name.as_str()).collect();
    let visible = names.iter().filter(|n| !hidden.contains(**n)).count();
    if !dry {
        // Only bodies the design still has are kept in the list, in the old order first.
        let mut list: Vec<String> = s.visibility.hidden_bodies.iter().filter(|n| hidden.contains(*n)).cloned().collect();
        let kept: BTreeSet<String> = list.iter().cloned().collect();
        list.extend(hidden.iter().filter(|n| !kept.contains(*n)).cloned());
        s.visibility.hidden_bodies = list;
        if unhide {
            s.visibility.hidden_occurrences.clear();
        }
        s.revision += 1;
    }
    Ok(json!({
        "shown": shown_by.iter().filter(|n| !hidden.contains(*n)).count(),
        "hidden": hidden.iter().filter(|n| names.contains(n.as_str())).count(),
        "visible": visible,
        "terms": terms.len(),
        "ignored": ignored,
        "dry_run": dry,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &[(&str, &str)]) -> Vec<(String, String)> {
        rows.iter().map(|(n, v)| (shortcut_key(n), v.to_string())).collect()
    }

    fn terms(text: &str, rows: &[(&str, &str)]) -> Vec<String> {
        let mut out = Vec::new();
        flatten(text, &table(rows), 0, &mut out).unwrap();
        out
    }

    #[test]
    fn splits_at_top_level_commas_and_semicolons_only() {
        assert_eq!(split_terms("a, b ;c,{x=1,y=2},[+t,u]"), ["a", "b", "c", "{x=1,y=2}", "[+t,u]"]);
        assert!(split_terms(" , ;").is_empty());
    }

    #[test]
    fn shortcuts_expand_only_as_whole_terms() {
        let rows = [("_S5", "-,+deck5"), ("_s500", "+hull")];
        assert_eq!(terms("U;_s5", &rows), ["U", "-", "+deck5"]);
        assert_eq!(terms("-x, _S5 ,-y", &rows), ["-x", "-", "+deck5", "-y"]);
        // A name that contains a shortcut name, a signed shortcut and braces stay literal.
        assert_eq!(terms("-wall_S500_x", &rows), ["-wall_S500_x"]);
        assert_eq!(terms("-_S5", &rows), ["-_S5"]);
        assert_eq!(terms("{_S5=1}", &rows), ["{_S5=1}"]);
        // Nested shortcuts expand; a loop is an error, not a hang.
        assert_eq!(terms("_a", &[("_a", "_b,+x"), ("_b", "-y")]), ["-y", "+x"]);
        let mut out = Vec::new();
        assert!(flatten("_a", &table(&[("_a", "_b"), ("_b", "_a")]), 0, &mut out).is_err());
    }

    #[test]
    fn terms_parse_to_their_operations() {
        assert_eq!(parse_term("-"), [Op::All { show: false }]);
        assert_eq!(parse_term("+"), [Op::All { show: true }]);
        assert_eq!(parse_term("+Deck 5"), [Op::Name { show: true, text: "deck 5".into() }]);
        assert_eq!(parse_term("-!@Hull1"), [Op::Overlap { show: false, outside: true, hull: "hull1".into() }]);
        assert_eq!(parse_term("+@h"), [Op::Overlap { show: true, outside: false, hull: "h".into() }]);
        assert_eq!(parse_term("@h"), [Op::Overlap { show: true, outside: false, hull: "h".into() }]);
        assert_eq!(parse_term("u"), [Op::Unhide]);
        assert_eq!(
            parse_term("{a=1, b=x}"),
            [Op::Assign { name: "a".into(), value: "1".into() }, Op::Assign { name: "b".into(), value: "x".into() }]
        );
        for host in ["V1", "view2", "V:Low corner", "C", "P", "L1", "SHOT:a@V2", "ORBIT:x@V1>V2:8", "[+tag]", "plain"] {
            assert!(matches!(parse_term(host).as_slice(), [Op::Ignored(_)]), "{host}");
        }
        // `-V1` is a name term (hide bodies named V1), not a view.
        assert_eq!(parse_term("-V1"), [Op::Name { show: false, text: "v1".into() }]);
    }

    fn run(s: &mut Session, id: &str, p: Value) -> Value {
        s.execute(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    #[test]
    fn rules_show_and_hide_bodies_by_name_and_shortcut() {
        let mut s = Session::default();
        for (i, name) in ["Hull_Main", "Deck1_White", "Deck2_White", "Lamp"].iter().enumerate() {
            run(&mut s, "solid.box", json!({"length": 10, "width": 10, "height": 10, "corner": [(i as f64) * 30.0, 0, 0]}));
            let body = s.model.state().bodies.last().unwrap().name.clone();
            run(&mut s, "body.rename", json!({"body": body, "name": name}));
        }
        let visible = |s: &mut Session, rule: &str| run(s, "view.show_hide", json!({"rule": rule}))["visible"].as_u64().unwrap();
        assert_eq!(visible(&mut s, "-"), 0);
        assert_eq!(visible(&mut s, "+deck"), 2);
        assert_eq!(visible(&mut s, "-deck2"), 1);
        assert_eq!(visible(&mut s, "+"), 4);
        assert_eq!(visible(&mut s, "-,+LAMP"), 1);
        // Terms build on each other; `;` steps do too.
        assert_eq!(visible(&mut s, "+;-lamp;-hull"), 2);
        // A shortcut is a named rule in the design's metadata, its name starting with `_`.
        run(&mut s, "document.metadata.set", json!({"key": SHORTCUTS_KEY, "value": {"_decks": "-,+deck"}}));
        assert_eq!(visible(&mut s, "_decks"), 2);
        assert_eq!(visible(&mut s, "-wall_decks_x"), 2, "a name containing the shortcut name is literal");
        // Unknown and host terms are reported, not run; a dry run changes nothing.
        let r = run(&mut s, "view.show_hide", json!({"rule": "-,V1,SHOT:a", "dry_run": true}));
        assert_eq!(r["ignored"], json!(["V1", "SHOT:a"]));
        assert_eq!(visible(&mut s, "+deck1"), 2);
        assert!(s.execute("view.show_hide", &json!({})).is_err());
        assert!(s.execute("view.show_hide", &json!({"rule": "+@nothere"})).is_err());
    }

    #[test]
    fn overlap_terms_follow_shared_volume_and_spare_the_hulls() {
        let mut s = Session::default();
        // Hull_A covers x 0..10 with Near; Far sits clear of both hulls.
        for (name, x) in [("Hull_A", 0.0), ("Near", 5.0), ("Far", 100.0)] {
            run(&mut s, "solid.box", json!({"length": 10, "width": 10, "height": 10, "corner": [x, 0, 0]}));
            let body = s.model.state().bodies.last().unwrap().name.clone();
            run(&mut s, "body.rename", json!({"body": body, "name": name}));
        }
        let state = |s: &mut Session, rule: &str| {
            run(s, "view.show_hide", json!({"rule": rule}));
            let st = s.world_state();
            let mut v: Vec<String> = st.bodies.iter().filter(|b| s.body_visible(&b.name)).map(|b| b.name.clone()).collect();
            v.sort();
            v
        };
        assert_eq!(state(&mut s, "+,-!@hull_a"), ["Hull_A", "Near"], "hiding what is outside the hull leaves the hull alone");
        assert_eq!(state(&mut s, "-,+@hull_a"), ["Near"], "showing what overlaps does not show the hull");
        assert_eq!(state(&mut s, "+,-@hull_a"), ["Far", "Hull_A"]);
        assert_eq!(state(&mut s, "-,+!@hull_a"), ["Far"]);
    }
}
