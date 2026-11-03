// SPDX-License-Identifier: Apache-2.0
//! A small JSON Schema (draft-07) validator: exactly the keywords the CycloneDX 1.5 schema and the
//! two files it references use, no more (`cargo xtask sbom` validates every SBOM with it).
//!
//! Supported: `$ref` (to `#/pointer`, another loaded document, or a pointer into one), `type`,
//! `enum`, `const`, `required`, `properties`, `additionalProperties`, `items` (schema or tuple
//! with `additionalItems`), `minItems`, `maxItems`, `uniqueItems`, `minLength`, `maxLength`,
//! `minimum`, `maximum`, `pattern` (a small regular-expression engine, see [`Regex`]), `format`
//! (`date-time` only; `iri-reference`, `idn-email` and the others are annotations, as they are
//! in the JSON Schema draft unless a validator opts in), `allOf`, `anyOf`, `oneOf`, `if`/`then`/
//! `else`. An unsupported `pattern` syntax is reported as an error rather than ignored, so a
//! schema update that needs more fails loudly. No external crate: a validator dependency would add
//! dozens of crates to a supply-chain tool.

use std::collections::HashMap;

use serde_json::Value;

/// A set of schema documents that may refer to each other by file name.
#[derive(Debug)]
pub struct Schemas {
    docs: HashMap<String, Value>,
}

impl Schemas {
    /// `(file name, JSON text)` pairs; `$ref`s name the files.
    pub fn new(docs: &[(&str, &str)]) -> Result<Self, String> {
        let mut map = HashMap::new();
        for (name, text) in docs {
            let value: Value =
                serde_json::from_str(text).map_err(|e| format!("schema {name}: {e}"))?;
            map.insert((*name).to_owned(), value);
        }
        Ok(Self { docs: map })
    }

    /// The problems of `instance` against the schema document `entry`; empty when valid.
    pub fn validate(&self, entry: &str, instance: &Value) -> Vec<String> {
        let mut errors = Vec::new();
        match self.docs.get_key_value(entry) {
            Some((name, root)) => self.check(name, root, instance, "", &mut errors),
            None => errors.push(format!("no schema document named {entry}")),
        }
        errors
    }

    /// The document `name`, for callers that read a list out of it (the SPDX licence ids).
    pub fn document(&self, name: &str) -> Option<&Value> {
        self.docs.get(name)
    }

    fn resolve<'a>(&'a self, doc: &'a str, reference: &str) -> Option<(&'a str, &'a Value)> {
        let (file, fragment) = reference.split_once('#').unwrap_or((reference, ""));
        let (name, root) = if file.is_empty() {
            self.docs.get_key_value(doc)?
        } else {
            self.docs.get_key_value(file)?
        };
        let node = if fragment.is_empty() {
            root
        } else {
            root.pointer(fragment)?
        };
        Some((name.as_str(), node))
    }

    fn valid(&self, doc: &str, schema: &Value, instance: &Value) -> bool {
        let mut errors = Vec::new();
        self.check(doc, schema, instance, "", &mut errors);
        errors.is_empty()
    }

    fn check(
        &self,
        doc: &str,
        schema: &Value,
        instance: &Value,
        path: &str,
        errors: &mut Vec<String>,
    ) {
        let Some(object) = schema.as_object() else {
            // `true` accepts everything; `false` accepts nothing.
            if schema == &Value::Bool(false) {
                errors.push(format!("{}: not allowed", at(path)));
            }
            return;
        };
        // Draft-07: a `$ref` replaces its siblings.
        if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
            match self.resolve(doc, reference) {
                Some((target_doc, target)) => {
                    self.check(target_doc, target, instance, path, errors)
                }
                None => errors.push(format!("{}: unresolved $ref {reference}", at(path))),
            }
            return;
        }

        if let Some(t) = object.get("type") {
            let names: Vec<&str> = match t {
                Value::String(s) => vec![s.as_str()],
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            if !names.iter().any(|n| type_matches(n, instance)) {
                errors.push(format!(
                    "{}: expected {}, found {}",
                    at(path),
                    names.join(" or "),
                    type_name(instance)
                ));
                return;
            }
        }
        if let Some(Value::Array(options)) = object.get("enum")
            && !options.contains(instance)
        {
            errors.push(format!(
                "{}: {} is not one of the {} allowed values",
                at(path),
                short(instance),
                options.len()
            ));
        }
        if let Some(constant) = object.get("const")
            && constant != instance
        {
            errors.push(format!("{}: must be {}", at(path), short(constant)));
        }

        match instance {
            Value::Object(map) => {
                for name in object
                    .get("required")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    if !map.contains_key(name) {
                        errors.push(format!("{}: missing required property {name}", at(path)));
                    }
                }
                let properties = object.get("properties").and_then(Value::as_object);
                for (key, value) in map {
                    let child = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                    match properties.and_then(|p| p.get(key)) {
                        Some(sub) => self.check(doc, sub, value, &child, errors),
                        None => match object.get("additionalProperties") {
                            Some(Value::Bool(false)) => {
                                errors.push(format!("{}: property {key} is not allowed", at(path)));
                            }
                            Some(sub @ Value::Object(_)) => {
                                self.check(doc, sub, value, &child, errors);
                            }
                            _ => {}
                        },
                    }
                }
            }
            Value::Array(items) => {
                if let Some(min) = object.get("minItems").and_then(Value::as_u64)
                    && (items.len() as u64) < min
                {
                    errors.push(format!("{}: fewer than {min} items", at(path)));
                }
                if let Some(max) = object.get("maxItems").and_then(Value::as_u64)
                    && (items.len() as u64) > max
                {
                    errors.push(format!("{}: more than {max} items", at(path)));
                }
                match object.get("items") {
                    Some(Value::Array(tuple)) => {
                        for (i, item) in items.iter().enumerate() {
                            let child = format!("{path}/{i}");
                            match tuple.get(i) {
                                Some(sub) => self.check(doc, sub, item, &child, errors),
                                None => match object.get("additionalItems") {
                                    Some(Value::Bool(false)) => errors.push(format!(
                                        "{}: item {i} is beyond the allowed positions",
                                        at(path)
                                    )),
                                    Some(sub @ Value::Object(_)) => {
                                        self.check(doc, sub, item, &child, errors);
                                    }
                                    _ => {}
                                },
                            }
                        }
                    }
                    Some(sub) => {
                        for (i, item) in items.iter().enumerate() {
                            self.check(doc, sub, item, &format!("{path}/{i}"), errors);
                        }
                    }
                    None => {}
                }
                if object.get("uniqueItems") == Some(&Value::Bool(true)) {
                    let mut seen = std::collections::HashSet::new();
                    for (i, item) in items.iter().enumerate() {
                        // Canonical text of the value: equal values print equally (serde_json
                        // maps are ordered), and hashing it keeps this linear.
                        if !seen.insert(item.to_string()) {
                            errors
                                .push(format!("{}: item {i} duplicates an earlier one", at(path)));
                        }
                    }
                }
            }
            Value::String(text) => {
                let length = text.chars().count() as u64;
                if let Some(min) = object.get("minLength").and_then(Value::as_u64)
                    && length < min
                {
                    errors.push(format!("{}: shorter than {min} characters", at(path)));
                }
                if let Some(max) = object.get("maxLength").and_then(Value::as_u64)
                    && length > max
                {
                    errors.push(format!("{}: longer than {max} characters", at(path)));
                }
                if let Some(pattern) = object.get("pattern").and_then(Value::as_str) {
                    match Regex::new(pattern) {
                        Ok(re) if re.is_match(text) => {}
                        Ok(_) => errors.push(format!(
                            "{}: {} does not match the pattern {pattern}",
                            at(path),
                            short(instance)
                        )),
                        Err(e) => {
                            errors.push(format!("{}: unsupported pattern {pattern}: {e}", at(path)))
                        }
                    }
                }
                if object.get("format").and_then(Value::as_str) == Some("date-time")
                    && !is_date_time(text)
                {
                    errors.push(format!(
                        "{}: {} is not an RFC 3339 date-time",
                        at(path),
                        short(instance)
                    ));
                }
            }
            Value::Number(number) => {
                if let (Some(min), Some(n)) = (
                    object.get("minimum").and_then(Value::as_f64),
                    number.as_f64(),
                ) && n < min
                {
                    errors.push(format!("{}: {n} is below the minimum {min}", at(path)));
                }
                if let (Some(max), Some(n)) = (
                    object.get("maximum").and_then(Value::as_f64),
                    number.as_f64(),
                ) && n > max
                {
                    errors.push(format!("{}: {n} is above the maximum {max}", at(path)));
                }
            }
            _ => {}
        }

        if let Some(Value::Array(all)) = object.get("allOf") {
            for sub in all {
                self.check(doc, sub, instance, path, errors);
            }
        }
        if let Some(Value::Array(any)) = object.get("anyOf")
            && !any.iter().any(|sub| self.valid(doc, sub, instance))
        {
            errors.push(format!(
                "{}: matches none of the {} alternatives",
                at(path),
                any.len()
            ));
        }
        if let Some(Value::Array(one)) = object.get("oneOf") {
            let matching = one
                .iter()
                .filter(|sub| self.valid(doc, sub, instance))
                .count();
            if matching != 1 {
                // Say why the closest alternative failed: the first one's problems.
                let detail = one
                    .iter()
                    .find_map(|sub| {
                        let mut e = Vec::new();
                        self.check(doc, sub, instance, path, &mut e);
                        e.into_iter().next()
                    })
                    .unwrap_or_default();
                errors.push(format!(
                    "{}: matches {matching} of the {} alternatives, exactly one is required ({detail})",
                    at(path),
                    one.len()
                ));
            }
        }
        if let Some(condition) = object.get("if") {
            let branch = if self.valid(doc, condition, instance) {
                object.get("then")
            } else {
                object.get("else")
            };
            if let Some(branch) = branch {
                self.check(doc, branch, instance, path, errors);
            }
        }
    }
}

fn at(path: &str) -> &str {
    if path.is_empty() { "(document)" } else { path }
}

fn short(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 60 {
        format!("{}...", text.chars().take(57).collect::<String>())
    } else {
        text
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_matches(name: &str, value: &Value) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        _ => false,
    }
}

/// RFC 3339 `date-time`: `YYYY-MM-DDTHH:MM:SS[.fraction](Z|+HH:MM|-HH:MM)`.
pub fn is_date_time(text: &str) -> bool {
    let b = text.as_bytes();
    let digits = |from: usize, n: usize| {
        b.get(from..from + n)
            .is_some_and(|s| s.iter().all(u8::is_ascii_digit))
    };
    let num = |from: usize, n: usize| -> u32 {
        text.get(from..from + n)
            .and_then(|s| s.parse().ok())
            .unwrap_or(u32::MAX)
    };
    if !(digits(0, 4)
        && b.get(4) == Some(&b'-')
        && digits(5, 2)
        && b.get(7) == Some(&b'-')
        && digits(8, 2)
        && matches!(b.get(10), Some(b'T' | b't' | b' '))
        && digits(11, 2)
        && b.get(13) == Some(&b':')
        && digits(14, 2)
        && b.get(16) == Some(&b':')
        && digits(17, 2))
    {
        return false;
    }
    if !(1..=12).contains(&num(5, 2))
        || !(1..=31).contains(&num(8, 2))
        || num(11, 2) > 23
        || num(14, 2) > 59
        || num(17, 2) > 60
    {
        return false;
    }
    let mut at = 19;
    if b.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while b.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == start {
            return false;
        }
    }
    match b.get(at) {
        Some(b'Z' | b'z') => at + 1 == b.len(),
        Some(b'+' | b'-') => {
            digits(at + 1, 2)
                && b.get(at + 3) == Some(&b':')
                && digits(at + 4, 2)
                && at + 6 == b.len()
                && num(at + 1, 2) <= 23
                && num(at + 4, 2) <= 59
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------
// A small regular-expression engine
// ---------------------------------------------------------------------------------------------

/// The regular-expression subset JSON Schema patterns in the CycloneDX schema use: literals, `.`,
/// character classes (ranges, negation, `\d`), `^`, `$`, groups (capturing or `(?:`), alternation,
/// and the quantifiers `*`, `+`, `?`, `{n}`, `{n,}`, `{n,m}`. Backtracking, search semantics
/// (a pattern without `^` may match anywhere), no lazy quantifiers, no lookaround, no
/// back-references: those are errors, not silently different behaviour.
#[derive(Debug)]
pub struct Regex {
    alternatives: Vec<Vec<Piece>>,
}

#[derive(Debug)]
struct Piece {
    node: Node,
    min: usize,
    max: usize,
}

#[derive(Debug)]
enum Node {
    Char(char),
    Any,
    Class {
        ranges: Vec<(char, char)>,
        negated: bool,
    },
    Group(Vec<Vec<Piece>>),
    Start,
    End,
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn alternatives(&mut self) -> Result<Vec<Vec<Piece>>, String> {
        let mut out = vec![self.sequence()?];
        while self.peek() == Some('|') {
            self.at += 1;
            out.push(self.sequence()?);
        }
        Ok(out)
    }

    fn sequence(&mut self) -> Result<Vec<Piece>, String> {
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let node = self.atom()?;
            let (min, max) = self.quantifier()?;
            out.push(Piece { node, min, max });
        }
        Ok(out)
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.peek().ok_or("unexpected end")?;
        self.at += 1;
        match c {
            '^' => Ok(Node::Start),
            '$' => Ok(Node::End),
            '.' => Ok(Node::Any),
            '(' => {
                if self.chars.get(self.at) == Some(&'?') {
                    if self.chars.get(self.at + 1) == Some(&':') {
                        self.at += 2;
                    } else {
                        return Err("lookaround and flags are not supported".to_owned());
                    }
                }
                let inner = self.alternatives()?;
                if self.peek() != Some(')') {
                    return Err("unclosed group".to_owned());
                }
                self.at += 1;
                Ok(Node::Group(inner))
            }
            '[' => self.class(),
            '\\' => {
                let e = self.peek().ok_or("dangling backslash")?;
                self.at += 1;
                match e {
                    'd' => Ok(Node::Class {
                        ranges: vec![('0', '9')],
                        negated: false,
                    }),
                    'w' => Ok(Node::Class {
                        ranges: vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')],
                        negated: false,
                    }),
                    's' => Ok(Node::Class {
                        ranges: vec![(' ', ' '), ('\t', '\r')],
                        negated: false,
                    }),
                    c if c.is_ascii_alphanumeric() => Err(format!("escape \\{c} is not supported")),
                    c => Ok(Node::Char(c)),
                }
            }
            '*' | '+' | '?' | '{' => Err(format!("nothing to repeat before {c}")),
            c => Ok(Node::Char(c)),
        }
    }

    fn class(&mut self) -> Result<Node, String> {
        let negated = self.peek() == Some('^');
        if negated {
            self.at += 1;
        }
        let mut ranges = Vec::new();
        let mut first = true;
        loop {
            let c = self.peek().ok_or("unclosed character class")?;
            self.at += 1;
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo = if c == '\\' {
                let e = self.peek().ok_or("dangling backslash")?;
                self.at += 1;
                if e == 'd' {
                    ranges.push(('0', '9'));
                    continue;
                }
                if e.is_ascii_alphanumeric() {
                    return Err(format!("escape \\{e} is not supported"));
                }
                e
            } else {
                c
            };
            if self.peek() == Some('-') && self.chars.get(self.at + 1).is_some_and(|n| *n != ']') {
                self.at += 1;
                let hi = self.peek().ok_or("unclosed character class")?;
                self.at += 1;
                if hi < lo {
                    return Err("reversed range".to_owned());
                }
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
        Ok(Node::Class { ranges, negated })
    }

    fn quantifier(&mut self) -> Result<(usize, usize), String> {
        let (min, max) = match self.peek() {
            Some('*') => (0, usize::MAX),
            Some('+') => (1, usize::MAX),
            Some('?') => (0, 1),
            Some('{') => {
                let close = self.chars[self.at..]
                    .iter()
                    .position(|c| *c == '}')
                    .ok_or("unclosed {")?;
                let body: String = self.chars[self.at + 1..self.at + close].iter().collect();
                self.at += close; // sits on the closing brace; advanced below
                let parse = |s: &str| s.trim().parse::<usize>().map_err(|_| "bad repeat count");
                match body.split_once(',') {
                    None => {
                        let n = parse(&body)?;
                        (n, n)
                    }
                    Some((lo, "")) => (parse(lo)?, usize::MAX),
                    Some((lo, hi)) => (parse(lo)?, parse(hi)?),
                }
            }
            _ => return Ok((1, 1)),
        };
        self.at += 1;
        if matches!(self.peek(), Some('?' | '+')) {
            return Err("lazy and possessive quantifiers are not supported".to_owned());
        }
        Ok((min, max))
    }
}

impl Regex {
    pub fn new(pattern: &str) -> Result<Self, String> {
        let mut parser = Parser {
            chars: pattern.chars().collect(),
            at: 0,
        };
        let alternatives = parser.alternatives()?;
        if parser.at != parser.chars.len() {
            return Err("unbalanced )".to_owned());
        }
        Ok(Self { alternatives })
    }

    /// Whether the pattern matches anywhere in `text`.
    pub fn is_match(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        (0..=chars.len()).any(|start| {
            self.alternatives
                .iter()
                .any(|alt| match_sequence(alt, &chars, start, &mut |_| true))
        })
    }
}

fn match_sequence(
    pieces: &[Piece],
    text: &[char],
    pos: usize,
    next: &mut dyn FnMut(usize) -> bool,
) -> bool {
    let Some((piece, rest)) = pieces.split_first() else {
        return next(pos);
    };
    match_repeat(piece, rest, text, pos, 0, next)
}

fn match_repeat(
    piece: &Piece,
    rest: &[Piece],
    text: &[char],
    pos: usize,
    count: usize,
    next: &mut dyn FnMut(usize) -> bool,
) -> bool {
    if count < piece.max {
        let more = match_node(&piece.node, text, pos, &mut |p| {
            // An empty repetition cannot make progress; stop it from looping.
            if p == pos && count >= piece.min {
                return false;
            }
            match_repeat(piece, rest, text, p, count + 1, next)
        });
        if more {
            return true;
        }
    }
    count >= piece.min && match_sequence(rest, text, pos, next)
}

fn match_node(node: &Node, text: &[char], pos: usize, next: &mut dyn FnMut(usize) -> bool) -> bool {
    match node {
        Node::Char(c) => text.get(pos) == Some(c) && next(pos + 1),
        Node::Any => pos < text.len() && text[pos] != '\n' && next(pos + 1),
        Node::Class { ranges, negated } => {
            text.get(pos)
                .is_some_and(|c| ranges.iter().any(|(lo, hi)| lo <= c && c <= hi) != *negated)
                && next(pos + 1)
        }
        Node::Start => pos == 0 && next(pos),
        Node::End => pos == text.len() && next(pos),
        Node::Group(alternatives) => alternatives
            .iter()
            .any(|alt| match_sequence(alt, text, pos, next)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn regex_handles_the_cyclonedx_patterns() {
        let serial =
            Regex::new("^urn:uuid:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
                .unwrap();
        assert!(serial.is_match("urn:uuid:3e671687-395b-41f5-a30f-a58921a69b79"));
        assert!(!serial.is_match("urn:uuid:3E671687-395b-41f5-a30f-a58921a69b79"));
        assert!(!serial.is_match("urn:uuid:3e671687-395b-41f5-a30f-a58921a69b7"));
        assert!(!serial.is_match("x urn:uuid:3e671687-395b-41f5-a30f-a58921a69b79"));

        let hash = Regex::new(
            "^([a-fA-F0-9]{32}|[a-fA-F0-9]{40}|[a-fA-F0-9]{64}|[a-fA-F0-9]{96}|[a-fA-F0-9]{128})$",
        )
        .unwrap();
        assert!(hash.is_match(&"ab".repeat(32)));
        assert!(hash.is_match(&"0".repeat(40)));
        assert!(!hash.is_match(&"a".repeat(63)));
        assert!(!hash.is_match(&"g".repeat(64)));

        let mime = Regex::new("^[-+a-z0-9.]+/[-+a-z0-9.]+$").unwrap();
        assert!(mime.is_match("application/vnd.cyclonedx+json"));
        assert!(!mime.is_match("application"));
        assert!(!mime.is_match("Application/json"));

        let locale = Regex::new("^([a-z]{2})(-[A-Z]{2})?$").unwrap();
        assert!(locale.is_match("en"));
        assert!(locale.is_match("en-GB"));
        assert!(!locale.is_match("en-gb"));
        assert!(!locale.is_match("eng"));

        let link = Regex::new(
            "^urn:cdx:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/[1-9][0-9]*#.+$",
        )
        .unwrap();
        assert!(link.is_match("urn:cdx:3e671687-395b-41f5-a30f-a58921a69b79/24#pkg:x/y@1"));
        assert!(!link.is_match("urn:cdx:3e671687-395b-41f5-a30f-a58921a69b79/0#x"));
        assert!(!link.is_match("urn:cdx:3e671687-395b-41f5-a30f-a58921a69b79/1#"));
    }

    #[test]
    fn regex_refuses_what_it_does_not_implement() {
        assert!(Regex::new("(?=a)b").is_err());
        assert!(Regex::new("a+?").is_err());
        assert!(Regex::new("\\p{L}").is_err());
        assert!(Regex::new("(a").is_err());
        assert!(Regex::new("[a").is_err());
        assert!(Regex::new("*a").is_err());
        // Unanchored patterns search; an empty repetition terminates.
        assert!(Regex::new("b+").unwrap().is_match("aabba"));
        assert!(Regex::new("(a*)*b").unwrap().is_match("aaab"));
        assert!(!Regex::new("(a*)*b").unwrap().is_match("aaac"));
        assert!(Regex::new("a|b").unwrap().is_match("xxb"));
        assert!(Regex::new("^[^0-9]+$").unwrap().is_match("abc"));
        assert!(!Regex::new("^[^0-9]+$").unwrap().is_match("ab1"));
    }

    #[test]
    fn date_times_follow_rfc_3339() {
        for ok in [
            "2026-11-02T17:50:17Z",
            "2026-11-02T17:50:17.123Z",
            "2026-11-02T17:50:17+01:00",
            "2026-11-02T17:50:17-05:30",
        ] {
            assert!(is_date_time(ok), "{ok}");
        }
        for bad in [
            "2026-11-02",
            "2026-11-02T17:50:17",
            "2026-13-02T17:50:17Z",
            "2026-11-02T25:50:17Z",
            "2026-11-02T17:50:17.Z",
            "2026-11-02T17:50:17Zjunk",
            "2026-11-02T17:50:17+0100",
            "yesterday",
        ] {
            assert!(!is_date_time(bad), "{bad}");
        }
    }

    fn schemas(root: &str) -> Schemas {
        Schemas::new(&[
            ("root.json", root),
            (
                "other.json",
                r##"{"definitions": {"small": {"type": "integer", "maximum": 5}}}"##,
            ),
        ])
        .unwrap()
    }

    fn problems(root: &str, instance: &Value) -> Vec<String> {
        schemas(root).validate("root.json", instance)
    }

    #[test]
    fn keywords_accept_and_reject() {
        let schema = r##"{
            "type": "object",
            "required": ["name"],
            "additionalProperties": false,
            "properties": {
                "name": {"type": "string", "minLength": 2, "pattern": "^[a-z]+$"},
                "n": {"$ref": "other.json#/definitions/small"},
                "tags": {"type": "array", "uniqueItems": true, "maxItems": 2, "items": {"enum": ["a", "b", "c"]}},
                "when": {"type": "string", "format": "date-time"},
                "pair": {"type": "array", "items": [{"type": "string"}], "additionalItems": false},
                "local": {"$ref": "#/definitions/flag"},
                "choice": {"oneOf": [{"type": "string"}, {"type": "integer"}]},
                "any": {"anyOf": [{"const": 1}, {"const": 2}]},
                "cond": {"if": {"const": "x"}, "then": {"minLength": 3}}
            },
            "definitions": {"flag": {"type": "boolean"}}
        }"##;
        let good = json!({
            "name": "ab", "n": 3, "tags": ["a", "b"], "when": "2026-11-02T00:00:00Z",
            "pair": ["x"], "local": true, "choice": 5, "any": 2, "cond": "y"
        });
        assert_eq!(problems(schema, &good), Vec::<String>::new());

        let cases = [
            (json!({}), "missing required property name"),
            (json!({"name": "a"}), "shorter than 2"),
            (json!({"name": "AB"}), "does not match the pattern"),
            (
                json!({"name": "ab", "extra": 1}),
                "property extra is not allowed",
            ),
            (json!({"name": "ab", "n": 6}), "above the maximum"),
            (json!({"name": "ab", "n": 1.5}), "expected integer"),
            (json!({"name": "ab", "tags": ["a", "a"]}), "duplicates"),
            (
                json!({"name": "ab", "tags": ["a", "b", "c"]}),
                "more than 2",
            ),
            (json!({"name": "ab", "tags": ["z"]}), "allowed values"),
            (json!({"name": "ab", "when": "tomorrow"}), "date-time"),
            (
                json!({"name": "ab", "pair": ["x", "y"]}),
                "beyond the allowed",
            ),
            (json!({"name": "ab", "local": "yes"}), "expected boolean"),
            (json!({"name": "ab", "choice": true}), "exactly one"),
            (json!({"name": "ab", "any": 3}), "none of the 2"),
            (json!({"name": "ab", "cond": "x"}), "shorter than 3"),
            (json!([]), "expected object"),
        ];
        for (instance, expected) in cases {
            let found = problems(schema, &instance);
            assert!(
                found.iter().any(|p| p.contains(expected)),
                "{instance}: wanted {expected:?}, got {found:?}"
            );
        }
        // The path names where the problem is.
        let found = problems(schema, &json!({"name": "ab", "tags": ["a", "z"]}));
        assert!(found.iter().any(|p| p.starts_with("/tags/1:")), "{found:?}");
    }

    #[test]
    fn unresolved_references_and_unknown_patterns_are_errors() {
        let found = problems(r##"{"$ref": "nowhere.json"}"##, &json!(1));
        assert!(found[0].contains("unresolved"));
        let found = problems(r##"{"pattern": "(?=x)"}"##, &json!("x"));
        assert!(found[0].contains("unsupported pattern"));
        assert!(
            Schemas::new(&[("a.json", "{")])
                .unwrap_err()
                .contains("a.json")
        );
    }
}
