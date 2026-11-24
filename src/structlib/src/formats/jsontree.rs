//! A position-tracking, tolerant JSON parser for the `json` stream.
//!
//! `serde_json` reports no source positions, and the `json` emitter needs the
//! exact byte (and so line) range of every value, so this hand-written
//! recursive-descent parser keeps the offsets. It is deliberately tolerant of
//! the JSONC dialect real config files use (`tsconfig.json`, `.jsonc`,
//! `opencode.jsonc`): `//` line comments, `/* */` block comments and trailing
//! commas before `}`/`]` are all accepted, and a leading UTF-8 BOM is skipped.
//!
//! It never panics and never fails the scan: on a syntax error it stops,
//! returning everything parsed so far, with every value still open at the
//! failure point flagged [`JNode::broken`] and ending at the failure offset.

/// One parsed JSON value and its exact byte range `[start, end)`.
#[derive(Debug, Clone)]
pub(crate) struct JNode {
    pub(crate) val: JVal,
    pub(crate) start: usize,
    pub(crate) end: usize,
    /// True when a syntax error cut this value short (it, or a descendant,
    /// was still open at the failure point).
    pub(crate) broken: bool,
}

/// An object member: its decoded key, the byte offset of the key's opening
/// quote, and its value.
#[derive(Debug, Clone)]
pub(crate) struct Member {
    pub(crate) key: String,
    pub(crate) key_start: usize,
    pub(crate) value: JNode,
}

#[derive(Debug, Clone)]
pub(crate) enum JVal {
    Object(Vec<Member>),
    Array(Vec<JNode>),
    Str(String),
    /// The number's source text, verbatim.
    Num(String),
    Bool(bool),
    Null,
}

impl JNode {
    /// The JSON type name: `object`, `array`, `string`, `number`, `boolean`
    /// or `null`.
    pub(crate) fn type_name(&self) -> &'static str {
        match self.val {
            JVal::Object(_) => "object",
            JVal::Array(_) => "array",
            JVal::Str(_) => "string",
            JVal::Num(_) => "number",
            JVal::Bool(_) => "boolean",
            JVal::Null => "null",
        }
    }

    pub(crate) fn is_scalar(&self) -> bool {
        !matches!(self.val, JVal::Object(_) | JVal::Array(_))
    }

    /// The member value for `key` when this node is an object.
    pub(crate) fn get(&self, key: &str) -> Option<&JNode> {
        match &self.val {
            JVal::Object(members) => members.iter().find(|m| m.key == key).map(|m| &m.value),
            _ => None,
        }
    }

    /// The node as a `serde_json::Value` (numbers that do not fit a JSON
    /// number are kept as their source string).
    pub(crate) fn to_value(&self) -> serde_json::Value {
        use serde_json::Value;
        match &self.val {
            JVal::Object(members) => Value::Object(
                members
                    .iter()
                    .map(|m| (m.key.clone(), m.value.to_value()))
                    .collect(),
            ),
            JVal::Array(items) => Value::Array(items.iter().map(JNode::to_value).collect()),
            JVal::Str(s) => Value::String(s.clone()),
            JVal::Num(n) => serde_json::from_str::<Value>(n).unwrap_or(Value::String(n.clone())),
            JVal::Bool(b) => Value::Bool(*b),
            JVal::Null => Value::Null,
        }
    }
}

/// Nesting beyond this depth is treated as a syntax error, so a hostile file
/// can never overflow the stack.
const MAX_NESTING: usize = 512;

/// Parses `text`. `None` when no value starts the document (an empty or
/// comment-only file, or garbage at the first token).
pub(crate) fn parse(text: &str) -> Option<JNode> {
    let mut p = Parser {
        s: text.as_bytes(),
        text,
        pos: 0,
        failed: false,
    };
    if text.starts_with('\u{feff}') {
        p.pos = '\u{feff}'.len_utf8();
    }
    p.skip_ws();
    if p.pos >= p.s.len() {
        return None;
    }
    p.value(0)
}

struct Parser<'a> {
    s: &'a [u8],
    text: &'a str,
    pos: usize,
    /// Set at the first syntax error; every open value then unwinds.
    failed: bool,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    /// Skips whitespace and `//` / `/* */` comments. An unterminated block
    /// comment runs to end of input.
    fn skip_ws(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.pos += 1,
                Some(b'/') if self.s.get(self.pos + 1) == Some(&b'/') => {
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                Some(b'/') if self.s.get(self.pos + 1) == Some(&b'*') => {
                    self.pos += 2;
                    loop {
                        match self.peek() {
                            None => break,
                            Some(b'*') if self.s.get(self.pos + 1) == Some(&b'/') => {
                                self.pos += 2;
                                break;
                            }
                            Some(_) => self.pos += 1,
                        }
                    }
                }
                _ => return,
            }
        }
    }

    fn fail(&mut self) {
        self.failed = true;
    }

    /// Parses one value at the current position (whitespace already skipped).
    /// `None` only when no value could start here; the failure flag is set.
    fn value(&mut self, nesting: usize) -> Option<JNode> {
        if nesting > MAX_NESTING {
            self.fail();
            return None;
        }
        let start = self.pos;
        let leaf = |val, end| {
            Some(JNode {
                val,
                start,
                end,
                broken: false,
            })
        };
        let Some(c) = self.peek() else {
            self.fail();
            return None;
        };
        match c {
            b'{' => Some(self.object(nesting)),
            b'[' => Some(self.array(nesting)),
            b'"' => {
                let (s, end, closed) = read_string(self.text, start);
                self.pos = end;
                if !closed {
                    self.fail();
                }
                Some(JNode {
                    val: JVal::Str(s),
                    start,
                    end,
                    broken: !closed,
                })
            }
            b't' if self.s[start..].starts_with(b"true") => {
                self.pos += 4;
                leaf(JVal::Bool(true), self.pos)
            }
            b'f' if self.s[start..].starts_with(b"false") => {
                self.pos += 5;
                leaf(JVal::Bool(false), self.pos)
            }
            b'n' if self.s[start..].starts_with(b"null") => {
                self.pos += 4;
                leaf(JVal::Null, self.pos)
            }
            b'-' | b'0'..=b'9' => {
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E') {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                leaf(JVal::Num(self.text[start..self.pos].to_string()), self.pos)
            }
            _ => {
                self.fail();
                None
            }
        }
    }

    fn object(&mut self, nesting: usize) -> JNode {
        let start = self.pos;
        self.pos += 1; // `{`
        let mut members = Vec::new();
        let mut broken = false;
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                Some(b'"') => {}
                _ => {
                    self.fail();
                    broken = true;
                    break;
                }
            }
            let key_start = self.pos;
            let (key, end, closed) = read_string(self.text, key_start);
            self.pos = end;
            self.skip_ws();
            if !closed || self.peek() != Some(b':') {
                self.fail();
                broken = true;
                break;
            }
            self.pos += 1;
            self.skip_ws();
            let Some(value) = self.value(nesting + 1) else {
                broken = true;
                break;
            };
            let child_broken = value.broken;
            members.push(Member {
                key,
                key_start,
                value,
            });
            if child_broken || self.failed {
                broken = true;
                break;
            }
            self.skip_ws();
            match self.peek() {
                // A trailing comma before `}` is tolerated by the next turn.
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => {
                    self.fail();
                    broken = true;
                    break;
                }
            }
        }
        JNode {
            val: JVal::Object(members),
            start,
            end: self.pos,
            broken,
        }
    }

    fn array(&mut self, nesting: usize) -> JNode {
        let start = self.pos;
        self.pos += 1; // `[`
        let mut items = Vec::new();
        let mut broken = false;
        loop {
            self.skip_ws();
            if self.peek() == Some(b']') {
                self.pos += 1;
                break;
            }
            let Some(item) = self.value(nesting + 1) else {
                broken = true;
                break;
            };
            let child_broken = item.broken;
            items.push(item);
            if child_broken || self.failed {
                broken = true;
                break;
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => {
                    self.fail();
                    broken = true;
                    break;
                }
            }
        }
        JNode {
            val: JVal::Array(items),
            start,
            end: self.pos,
            broken,
        }
    }
}

/// Reads the JSON string whose opening quote is at `start`: the decoded text,
/// the index just past the closing quote (end of input when unterminated),
/// and whether it was closed. `\uXXXX` surrogate pairs are combined.
pub(crate) fn read_string(text: &str, start: usize) -> (String, usize, bool) {
    let mut s = String::new();
    let mut iter = text[start + 1..].char_indices();
    let mut pending_high: Option<u32> = None;
    while let Some((off, c)) = iter.next() {
        match c {
            '"' => return (s, start + 1 + off + 1, true),
            '\\' => {
                let Some((_, e)) = iter.next() else { break };
                match e {
                    'n' => s.push('\n'),
                    't' => s.push('\t'),
                    'r' => s.push('\r'),
                    'b' => s.push('\u{8}'),
                    'f' => s.push('\u{c}'),
                    'u' => {
                        let hex: String = iter.by_ref().take(4).map(|(_, h)| h).collect();
                        let Ok(cp) = u32::from_str_radix(&hex, 16) else {
                            continue;
                        };
                        if (0xD800..0xDC00).contains(&cp) {
                            pending_high = Some(cp);
                            continue;
                        }
                        let cp = match pending_high.take() {
                            Some(hi) if (0xDC00..0xE000).contains(&cp) => {
                                0x10000 + ((hi - 0xD800) << 10) + (cp - 0xDC00)
                            }
                            _ => cp,
                        };
                        s.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                    }
                    other => s.push(other),
                }
            }
            _ => s.push(c),
        }
    }
    (s, text.len(), false)
}
