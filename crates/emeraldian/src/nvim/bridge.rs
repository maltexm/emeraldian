//! Rust side of the bridge: what `bridge.lua` tells emeraldian and how it is
//! injected.
//!
//! Kept free of `crate::` paths, like `session.rs`, so `tests/nvim_bridge.rs`
//! can include it by path.
//!
//! # Wire format
//!
//! `bridge.lua` sends RPC notifications named `emeraldian.<event>`:
//!
//! | method                 | params                                        |
//! |------------------------|-----------------------------------------------|
//! | `emeraldian.follow`    | `{ note, heading?, block?, source? }`         |
//! | `emeraldian.written`   | `path`                                        |
//! | `emeraldian.modified`  | `path`, `bool`                                |
//! | `emeraldian.entered`   | `path`                                        |
//! | `emeraldian.action`    | `name` (one of [`ACTIONS`])                   |
//! | `emeraldian.leave`     | —                                             |
//!
//! and makes one request, `emeraldian.notes`, answered with the names a
//! `[[link]]` can be completed to (see `Session::set_notes`).

use std::path::{Component, Path, PathBuf};

use rmpv::Value;

/// The script that is injected into nvim at startup.
pub const LUA: &str = include_str!("bridge.lua");

/// What `:Emeraldian <action>` accepts. The list is handed to the script when
/// it is injected, so this is the only place the names are written down.
pub const ACTIONS: &[&str] = &[
    "switcher",
    "search",
    "graph",
    "local_graph",
    "chat",
    "explorer",
    "backlinks",
    "outline",
    "focus_left",
    "focus_right",
    "new",
    "daily",
];

/// Where inside a note a link points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    /// `[[Note#Heading]]`
    Heading(String),
    /// `[[Note#^block-id]]`; the id without the `^`.
    Block(String),
}

/// A link to follow.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinkTarget {
    /// What the link says: a note name, a vault-relative path, or — for a
    /// markdown link — a path relative to [`LinkTarget::source`]. Never empty
    /// for a link that was written in a note (`[[#Heading]]` names its own
    /// note).
    pub note: String,
    pub anchor: Option<Anchor>,
    /// Set when `note` is to be read relative to the folder of this note: the
    /// way a markdown link `[text](../Beta.md)` is. A wikilink is looked up in
    /// the whole vault, as the index does when it records the link, and
    /// leaves this unset.
    pub source: Option<PathBuf>,
}

impl LinkTarget {
    /// A link that is just a name: what the graph, `:e` and the daily note
    /// follow.
    pub fn plain(note: impl Into<String>) -> LinkTarget {
        LinkTarget {
            note: note.into(),
            anchor: None,
            source: None,
        }
    }
}

/// What the injected bridge tells emeraldian about the notes being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeEvent {
    /// `<CR>`, `gf` or `<C-]>` on a link.
    Follow(LinkTarget),
    /// A note was written (by `:w`, auto-save, anything).
    Written(PathBuf),
    /// A note's buffer became modified or clean.
    Modified(PathBuf, bool),
    /// nvim now shows this note (`:e`, `:bnext`, `gf`, a jump back).
    Entered(PathBuf),
    /// `:Emeraldian <action>`; the name is one of [`ACTIONS`].
    Action(String),
    /// `:q`, `:wq` or `:x` on the last window: back to the reading view,
    /// nvim stays.
    LeaveEditing,
}

/// The arguments the script is called with: nvim's channel for this client
/// and its options.
#[must_use]
pub fn inject_args(channel: Value, root: &Path, auto_save: bool) -> Value {
    let options = Value::Map(vec![
        (
            Value::from("root"),
            Value::from(root.to_string_lossy().into_owned()),
        ),
        (Value::from("auto_save"), Value::from(auto_save)),
        (
            Value::from("actions"),
            Value::Array(ACTIONS.iter().map(|name| Value::from(*name)).collect()),
        ),
    ]);
    Value::Array(vec![channel, options])
}

/// The bridge event a notification stands for, or `None` for anything that is
/// not one (or is malformed: a wrong notification is dropped, not guessed at).
#[must_use]
pub fn parse_notification(method: &str, params: &[Value]) -> Option<BridgeEvent> {
    let event = method.strip_prefix("emeraldian.")?;
    let text = |index: usize| params.get(index).and_then(Value::as_str);
    match event {
        "follow" => parse_target(params.first()?).map(BridgeEvent::Follow),
        "written" => Some(BridgeEvent::Written(PathBuf::from(text(0)?))),
        "modified" => Some(BridgeEvent::Modified(
            PathBuf::from(text(0)?),
            params.get(1)?.as_bool()?,
        )),
        "entered" => Some(BridgeEvent::Entered(PathBuf::from(text(0)?))),
        "action" => Some(BridgeEvent::Action(text(0)?.to_string())),
        "leave" => Some(BridgeEvent::LeaveEditing),
        _ => None,
    }
}

fn parse_target(value: &Value) -> Option<LinkTarget> {
    let map = value.as_map()?;
    let field = |key: &str| {
        map.iter()
            .find(|(name, _)| name.as_str() == Some(key))
            .and_then(|(_, value)| value.as_str())
            .map(str::to_string)
    };
    let note = field("note").filter(|note| !note.is_empty())?;
    let anchor = match (field("heading"), field("block")) {
        (Some(heading), _) => Some(Anchor::Heading(heading)),
        (None, Some(block)) => Some(Anchor::Block(block)),
        (None, None) => None,
    };
    Some(LinkTarget {
        note,
        anchor,
        source: field("source").map(PathBuf::from),
    })
}

/// A relative link `note`, written in `source`, as a vault-relative path with
/// `/` separators; `None` when it leads out of the vault.
///
/// Purely lexical (`.` and `..` are folded, symlinks are not followed), which
/// is what a link means; a leading `/` is the vault root.
#[must_use]
pub fn relative_to(source: &Path, root: &Path, note: &str) -> Option<String> {
    let dir = source.parent()?;
    let base = dir
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .ok()
        // The note and the vault may be spelled differently (a symlinked
        // vault folder); compare them as the file system sees them.
        .or_else(|| {
            let root = std::fs::canonicalize(root).ok()?;
            let dir = std::fs::canonicalize(dir).ok()?;
            dir.strip_prefix(&root).map(Path::to_path_buf).ok()
        })?;

    let mut parts: Vec<String> = if note.starts_with('/') {
        Vec::new()
    } else {
        base.components()
            .filter_map(|part| match part {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect()
    };
    for segment in note.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name.to_string()),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// The `kind` and text the script's `goto_anchor` takes.
#[must_use]
pub fn anchor_parts(anchor: &Anchor) -> (&'static str, &str) {
    match anchor {
        Anchor::Heading(text) => ("heading", text),
        Anchor::Block(text) => ("block", text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Value {
        Value::from(text)
    }

    fn target(fields: Vec<(&str, &str)>) -> Value {
        Value::Map(fields.into_iter().map(|(k, v)| (s(k), s(v))).collect())
    }

    #[test]
    fn parse_follow_with_a_heading() {
        let params = [target(vec![("note", "Folder/Alpha"), ("heading", "Sec")])];

        assert_eq!(
            parse_notification("emeraldian.follow", &params),
            Some(BridgeEvent::Follow(LinkTarget {
                note: "Folder/Alpha".to_string(),
                anchor: Some(Anchor::Heading("Sec".to_string())),
                source: None,
            }))
        );
    }

    #[test]
    fn parse_follow_with_a_block_and_a_source() {
        let params = [target(vec![
            ("note", "../B.md"),
            ("block", "blk1"),
            ("source", "/v/sub/a.md"),
        ])];

        assert_eq!(
            parse_notification("emeraldian.follow", &params),
            Some(BridgeEvent::Follow(LinkTarget {
                note: "../B.md".to_string(),
                anchor: Some(Anchor::Block("blk1".to_string())),
                source: Some(PathBuf::from("/v/sub/a.md")),
            }))
        );
    }

    #[test]
    fn parse_simple_events() {
        assert_eq!(
            parse_notification("emeraldian.written", &[s("/v/a.md")]),
            Some(BridgeEvent::Written(PathBuf::from("/v/a.md")))
        );
        assert_eq!(
            parse_notification("emeraldian.modified", &[s("/v/a.md"), Value::from(true)]),
            Some(BridgeEvent::Modified(PathBuf::from("/v/a.md"), true))
        );
        assert_eq!(
            parse_notification("emeraldian.entered", &[s("/v/a.md")]),
            Some(BridgeEvent::Entered(PathBuf::from("/v/a.md")))
        );
        assert_eq!(
            parse_notification("emeraldian.action", &[s("switcher")]),
            Some(BridgeEvent::Action("switcher".to_string()))
        );
        assert_eq!(
            parse_notification("emeraldian.leave", &[]),
            Some(BridgeEvent::LeaveEditing)
        );
    }

    #[test]
    fn parse_drops_what_is_not_ours_or_malformed() {
        assert_eq!(parse_notification("redraw", &[]), None);
        assert_eq!(parse_notification("emeraldian.unheard_of", &[]), None);
        assert_eq!(parse_notification("emeraldian.written", &[]), None);
        assert_eq!(
            parse_notification("emeraldian.modified", &[s("/v/a.md")]),
            None
        );
        assert_eq!(
            parse_notification("emeraldian.follow", &[target(vec![("note", "")])]),
            None
        );
        assert_eq!(parse_notification("emeraldian.follow", &[s("Alpha")]), None);
    }

    #[test]
    fn relative_links_resolve_from_the_source_folder() {
        let root = Path::new("/v");
        let source = Path::new("/v/sub/a.md");

        assert_eq!(
            relative_to(source, root, "../B.md").as_deref(),
            Some("B.md")
        );
        assert_eq!(
            relative_to(source, root, "C.md").as_deref(),
            Some("sub/C.md")
        );
        assert_eq!(
            relative_to(source, root, "./d/E.md").as_deref(),
            Some("sub/d/E.md")
        );
        assert_eq!(
            relative_to(source, root, "/Top.md").as_deref(),
            Some("Top.md")
        );
        assert_eq!(
            relative_to(Path::new("/v/a.md"), root, "x.md").as_deref(),
            Some("x.md")
        );
    }

    #[test]
    fn relative_links_cannot_leave_the_vault() {
        let root = Path::new("/v");

        assert_eq!(
            relative_to(Path::new("/v/sub/a.md"), root, "../../x.md"),
            None
        );
        assert_eq!(relative_to(Path::new("/v/a.md"), root, "../x.md"), None);
        // A source outside the vault has no vault-relative folder at all.
        assert_eq!(
            relative_to(Path::new("/elsewhere/a.md"), root, "x.md"),
            None
        );
    }

    #[test]
    fn inject_args_carry_the_actions_and_options() {
        let args = inject_args(Value::from(3), Path::new("/v"), true);
        let args = args.as_array().expect("array");

        assert_eq!(args[0].as_i64(), Some(3));
        let options = args[1].as_map().expect("options");
        let get = |key: &str| {
            options
                .iter()
                .find(|(name, _)| name.as_str() == Some(key))
                .map(|(_, value)| value)
        };
        assert_eq!(get("root").and_then(Value::as_str), Some("/v"));
        assert_eq!(get("auto_save").and_then(Value::as_bool), Some(true));
        assert_eq!(
            get("actions").and_then(Value::as_array).map(Vec::len),
            Some(ACTIONS.len())
        );
    }
}
