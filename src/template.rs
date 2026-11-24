//! `[[field]]` markers: blanks in a stored message that are filled in through a modal each time
//! it is sent. Distinct from `{placeholders}`, which resolve on their own.
//!
//! Double brackets are used because single `[...]` is a Markdown link and `<...>` is a mention.

use serde_json::Value;

pub const OPEN: &str = "[[";
pub const CLOSE: &str = "]]";
/// A Discord modal holds at most this many inputs.
pub const MAX_FIELDS: usize = 5;
pub const MAX_FIELD_NAME_CHARS: usize = 32;

/// True for `a-z`, `0-9` and `_` names of a sensible length.
pub fn is_valid_field_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_FIELD_NAME_CHARS
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Every distinct field in the document, in order of first appearance. Invalid names are
/// included so callers can report them.
pub fn fields(value: &Value) -> Vec<String> {
    let mut found = Vec::new();
    for_each_string(value, &mut |text| {
        for name in markers(text) {
            if !found.iter().any(|f| f == name) {
                found.push(name.to_owned());
            }
        }
    });
    found
}

/// Replaces every `[[field]]` that has a value, in every string of the document.
pub fn fill(value: &mut Value, lookup: impl Fn(&str) -> Option<String>) {
    map_strings(value, &|text| {
        substitute(text, OPEN, CLOSE, |name| lookup(name))
    });
}

/// Names of the `[[...]]` markers in one string, left to right, duplicates included.
fn markers(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        while let Some(start) = rest.find(OPEN) {
            let after = &rest[start + OPEN.len()..];
            let Some(end) = after.find(CLOSE) else {
                rest = "";
                return None;
            };
            let name = &after[..end];
            if is_marker_name(name) {
                rest = &after[end + CLOSE.len()..];
                return Some(name);
            }
            rest = after;
        }
        None
    })
}

fn is_marker_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Replaces `open name close` with `lookup(name)` wherever the lookup succeeds; everything else is
/// copied verbatim. Shared by placeholders and template fields so both behave the same.
pub fn substitute(
    text: &str,
    open: &str,
    close: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        out.push_str(&rest[..start]);
        let after = &rest[start + open.len()..];
        let name_len = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        let name = &after[..name_len];
        match after[name_len..].strip_prefix(close) {
            Some(tail) if !name.is_empty() => match lookup(name) {
                Some(value) => {
                    out.push_str(&value);
                    rest = tail;
                }
                None => {
                    out.push_str(open);
                    rest = after;
                }
            },
            _ => {
                out.push_str(open);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn for_each_string(value: &Value, f: &mut impl FnMut(&str)) {
    match value {
        Value::String(s) => f(s),
        Value::Array(items) => items.iter().for_each(|v| for_each_string(v, f)),
        Value::Object(map) => map.values().for_each(|v| for_each_string(v, f)),
        _ => {}
    }
}

pub fn map_strings(value: &mut Value, f: &impl Fn(&str) -> String) {
    match value {
        Value::String(s) => *s = f(s),
        Value::Array(items) => items.iter_mut().for_each(|v| map_strings(v, f)),
        Value::Object(map) => map.values_mut().for_each(|v| map_strings(v, f)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_fields_in_order_without_duplicates() {
        let v = serde_json::json!({
            "content": "Hi [[name]], [[reason]] [[name]] [[not closed",
            "embeds": [{"title": "[[title]] [not a field] [[ spaced ]]"}]
        });
        assert_eq!(fields(&v), ["name", "reason", "title"]);
    }

    #[test]
    fn fills_known_and_keeps_unknown() {
        let mut v = serde_json::json!("[[a]] and [[b]] and [[a]] [link](url)");
        fill(&mut v, |n| (n == "a").then(|| "A".to_owned()));
        assert_eq!(v, "A and [[b]] and A [link](url)");
    }

    #[test]
    fn substitute_handles_edge_cases() {
        let lookup = |n: &str| (n == "x").then(|| "X".to_owned());
        assert_eq!(substitute("{x}{}{y}{x", "{", "}", lookup), "X{}{y}{x");
        assert_eq!(substitute("[[x]][[]]", "[[", "]]", lookup), "X[[]]");
    }
}
