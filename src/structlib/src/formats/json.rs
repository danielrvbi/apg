//! The `json` emitter: a bounded tree of the document's objects and values.
//!
//! Parsed by the tolerant, position-tracking [`super::jsontree`] parser (so
//! JSONC — `tsconfig.json`, `.jsonc` — works), then walked into `Struct`
//! records:
//!
//! - an object member is a Struct named by its key, recursed to
//!   `max_depth` (a top-level key is depth 1);
//! - a scalar is a leaf carrying its `value`;
//! - a list of scalars, of lists, or of mixed values is one leaf carrying the
//!   compact JSON of its first `max_items` elements as `items`;
//! - a list of objects becomes one Struct per element, a sibling of the list's
//!   key, named `<key>[id=foo]` — keyed by the first of `id`, `name` or `key`
//!   the element has, else `<key>[<index>]` — capped at `max_items`;
//! - every Struct carries its RFC 6901 `pointer`, its JSON `type` and its exact
//!   byte/line range;
//! - `truncated` (`depth`, `items`, `nodes`, `value` or `parse`) marks where a
//!   limit — or a syntax error — cut the tree, so an agent knows to read those
//!   lines itself.

use std::collections::BTreeMap;
use std::path::Path;

use crate::builder::{Structure, StructureBuilder};
use crate::config::JsonOptions;
use crate::lines::{line_index_at, source_lines, SourceLine, Span};

use super::jsontree::{parse, JNode, JVal, Member};

/// The longest scalar `value` kept verbatim (in chars); longer values are cut
/// and marked `truncated=value`.
const MAX_VALUE_CHARS: usize = 256;

/// The longest `items` rendering kept (in bytes); a longer one is cut back to
/// fewer elements and marked `truncated=items`.
const MAX_ITEMS_BYTES: usize = 4096;

/// The `json` emitter with the default options (the lockfile depth-1 default
/// keyed by the file name) — the pinned `(path, bytes, id_prefix, next_id)`
/// dispatch shape. `run` resolves per-file options and calls
/// [`emit_json_with`]; this default form serves the in-memory tests.
#[cfg(test)]
pub(crate) fn emit_json(
    path: &Path,
    bytes: &[u8],
    id_prefix: &str,
    next_id: &mut u64,
) -> Structure {
    let opts = crate::config::JsonConfig::default().resolve(&path.to_string_lossy());
    emit_json_with(path, bytes, id_prefix, next_id, opts)
}

/// The `json` emitter under explicit tree limits.
pub(crate) fn emit_json_with(
    path: &Path,
    bytes: &[u8],
    id_prefix: &str,
    next_id: &mut u64,
    opts: JsonOptions,
) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut w = Walker {
        b: StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id),
        lines: &lines,
        opts,
        last_top: None,
    };
    if let Some(root) = parse(&text) {
        if !lines.is_empty() {
            w.root(&root);
        }
    }
    w.b.finish()
}

struct Walker<'a, 'b> {
    b: StructureBuilder<'b>,
    lines: &'a [SourceLine<'a>],
    opts: JsonOptions,
    /// The id of the last file-rooted Struct, marked when the node budget
    /// runs out with no enclosing Struct to mark.
    last_top: Option<String>,
}

/// A Struct's `(id, fqn)`, the parent handle for its children.
type Handle = (String, String);

impl Walker<'_, '_> {
    fn root(&mut self, root: &JNode) {
        match &root.val {
            JVal::Object(members) => {
                self.members(None, members, "", 1);
            }
            JVal::Array(items) if is_object_list(items) => {
                self.object_list(None, "", items, "", 1);
            }
            // A scalar or non-object-list root is one node, named `$`.
            _ => {
                self.value(None, "$", root.start, root, "", 1);
            }
        }
        if root.broken {
            if let Some(id) = self.last_top.clone() {
                self.b.mark(&id, "truncated", "parse");
            }
        }
    }

    /// True when the per-file node budget is spent; marks `parent` (or the
    /// last top-level node) `truncated=nodes`.
    fn exhausted(&mut self, parent: Option<&Handle>) -> bool {
        if self.b.len() < self.opts.max_nodes as usize {
            return false;
        }
        let target = parent.map(|(id, _)| id.clone()).or(self.last_top.clone());
        if let Some(id) = target {
            self.b.mark(&id, "truncated", "nodes");
        }
        true
    }

    /// Emits an object's members under `parent`. Returns `false` when the node
    /// budget ran out (the walk stops).
    fn members(
        &mut self,
        parent: Option<&Handle>,
        members: &[Member],
        ptr: &str,
        depth: u32,
    ) -> bool {
        for m in members {
            let child_ptr = format!("{ptr}/{}", escape_pointer(&m.key));
            let ok = match &m.value.val {
                JVal::Array(items) if is_object_list(items) => {
                    self.object_list(parent, &m.key, items, &child_ptr, depth)
                }
                _ => self.value(parent, &m.key, m.key_start, &m.value, &child_ptr, depth),
            };
            if !ok {
                return false;
            }
        }
        true
    }

    /// Emits one value as a Struct named `name`, spanning `start..node.end`.
    fn value(
        &mut self,
        parent: Option<&Handle>,
        name: &str,
        start: usize,
        node: &JNode,
        ptr: &str,
        depth: u32,
    ) -> bool {
        if self.exhausted(parent) {
            return false;
        }
        let mut props = base_props(ptr, node);
        match &node.val {
            JVal::Array(items) => self.items_props(&mut props, items),
            JVal::Object(_) => {}
            _ => scalar_props(&mut props, node),
        }
        let h = self.add(parent, name, start, node.end, props);
        if node.broken {
            self.b.mark(&h.0, "truncated", "parse");
        }
        if let JVal::Object(members) = &node.val {
            if !members.is_empty() {
                if depth >= self.opts.max_depth {
                    self.b.mark(&h.0, "truncated", "depth");
                } else if !self.members(Some(&h), members, ptr, depth + 1) {
                    return false;
                }
            }
        }
        true
    }

    /// Emits a list of objects: one Struct per element (up to `max_items`),
    /// each a sibling named `<key>[id=foo]` / `<key>[<index>]`.
    fn object_list(
        &mut self,
        parent: Option<&Handle>,
        key: &str,
        items: &[JNode],
        ptr: &str,
        depth: u32,
    ) -> bool {
        let cap = self.opts.max_items as usize;
        for (i, item) in items.iter().enumerate().take(cap) {
            if self.exhausted(parent) {
                return false;
            }
            let name = format!("{key}[{}]", element_key(item, i));
            let item_ptr = format!("{ptr}/{i}");
            let props = base_props(&item_ptr, item);
            let h = self.add(parent, &name, item.start, item.end, props);
            if item.broken {
                self.b.mark(&h.0, "truncated", "parse");
            }
            if i + 1 == cap && items.len() > cap {
                self.b.mark(&h.0, "truncated", "items");
                self.b.mark(&h.0, "count", &items.len().to_string());
            }
            if let JVal::Object(members) = &item.val {
                if !members.is_empty() {
                    if depth >= self.opts.max_depth {
                        self.b.mark(&h.0, "truncated", "depth");
                    } else if !self.members(Some(&h), members, &item_ptr, depth + 1) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// The `items` (compact JSON of the first `max_items` elements) and, when
    /// cut, `count` + `truncated=items` of a non-object list leaf.
    fn items_props(&self, props: &mut BTreeMap<String, String>, items: &[JNode]) {
        let mut take = items.len().min(self.opts.max_items as usize);
        let render = |n: usize| {
            serde_json::Value::Array(items[..n].iter().map(JNode::to_value).collect()).to_string()
        };
        let mut rendered = render(take);
        while rendered.len() > MAX_ITEMS_BYTES && take > 0 {
            take /= 2;
            rendered = render(take);
        }
        props.insert("items".into(), rendered);
        if take < items.len() {
            props.insert("count".into(), items.len().to_string());
            props.insert("truncated".into(), "items".into());
        }
    }

    fn add(
        &mut self,
        parent: Option<&Handle>,
        name: &str,
        start: usize,
        end: usize,
        props: BTreeMap<String, String>,
    ) -> Handle {
        let span = self.span(start, end);
        let h = self.b.add_with(
            parent.map(|(i, f)| (i.as_str(), f.as_str())),
            name,
            span,
            props,
        );
        if parent.is_none() {
            self.last_top = Some(h.0.clone());
        }
        h
    }

    /// The exact byte range `start..end` with the lines it covers.
    fn span(&self, start: usize, end: usize) -> Span {
        let first = line_index_at(self.lines, start);
        let last = line_index_at(self.lines, end.saturating_sub(1).max(start));
        Span {
            start: start as u32,
            end: end as u32,
            start_line: self.lines[first].no,
            end_line: self.lines[last].no,
        }
    }
}

/// True for a non-empty list whose every element is an object.
fn is_object_list(items: &[JNode]) -> bool {
    !items.is_empty() && items.iter().all(|i| matches!(i.val, JVal::Object(_)))
}

/// An object-list element's name key: `id=…`, `name=…` or `key=…` from the
/// first of those members holding a scalar, else its index.
fn element_key(item: &JNode, index: usize) -> String {
    for k in ["id", "name", "key"] {
        if let Some(v) = item.get(k) {
            if v.is_scalar() {
                return format!("{k}={}", scalar_text(v));
            }
        }
    }
    index.to_string()
}

/// `pointer` and `type`, carried by every node.
fn base_props(ptr: &str, node: &JNode) -> BTreeMap<String, String> {
    let mut p = BTreeMap::new();
    p.insert("pointer".into(), ptr.to_string());
    p.insert("type".into(), node.type_name().to_string());
    p
}

/// A scalar leaf's `value` (cut at [`MAX_VALUE_CHARS`], `truncated=value`).
fn scalar_props(props: &mut BTreeMap<String, String>, node: &JNode) {
    let text = scalar_text(node);
    if text.chars().count() > MAX_VALUE_CHARS {
        props.insert("value".into(), text.chars().take(MAX_VALUE_CHARS).collect());
        props.insert("truncated".into(), "value".into());
    } else {
        props.insert("value".into(), text);
    }
}

/// A scalar's text: a string decoded, a number verbatim, `true`/`false`/`null`.
fn scalar_text(node: &JNode) -> String {
    match &node.val {
        JVal::Str(s) => s.clone(),
        JVal::Num(n) => n.clone(),
        JVal::Bool(b) => b.to_string(),
        JVal::Null => "null".into(),
        _ => String::new(),
    }
}

/// Escapes one RFC 6901 reference token (`~` → `~0`, `/` → `~1`).
pub(crate) fn escape_pointer(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}
