//! Serialisation of component trees to iCalendar/vCard text and the TEXT
//! value escaping rules (RFC 5545 §3.1, §3.3.11; RFC 6350 §3.4).
//!
//! The writer is the last line of defence against content-line injection:
//! it refuses names that are not valid and raw values with control
//! characters, so no input can ever produce an extra line.

use crate::{
    Component, IcalError, Parameter, Property,
    parser::{MAX_NAME_LEN, is_name},
};

/// Maximum octets per physical line, excluding the line break.
const FOLD_AT: usize = 75;

/// Escapes a TEXT value: `\`, `;`, `,` and line breaks. Other control
/// characters (except TAB) are dropped, as they are not allowed in
/// iCalendar.
pub fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            '\t' => out.push('\t'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Reverses [`escape_text`]. Unknown escapes are kept as they are.
pub fn unescape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(c @ ('\\' | ';' | ',')) => out.push(c),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Splits an escaped value at unescaped occurrences of `separator`.
pub fn split_unescaped(value: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (i, c) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == separator {
            parts.push(&value[start..i]);
            start = i + c.len_utf8();
        }
    }
    parts.push(&value[start..]);
    parts
}

fn check_name(name: &str) -> Result<(), IcalError> {
    if name.is_empty() || name.len() > MAX_NAME_LEN || !is_name(name) {
        return Err(IcalError::Invalid("invalid property or component name"));
    }
    Ok(())
}

fn check_raw(value: &str) -> Result<(), IcalError> {
    if value.chars().any(|c| c.is_control() && c != '\t') {
        return Err(IcalError::Invalid("control character in value"));
    }
    Ok(())
}

fn write_parameter(parameter: &Parameter, line: &mut String) -> Result<(), IcalError> {
    check_name(&parameter.name)?;
    line.push(';');
    line.push_str(&parameter.name);
    line.push('=');
    for (i, value) in parameter.values.iter().enumerate() {
        check_raw(value)?;
        if i > 0 {
            line.push(',');
        }
        // A DQUOTE cannot be represented in a parameter value.
        let value = value.replace('"', "'");
        if value.contains([':', ';', ',']) {
            line.push('"');
            line.push_str(&value);
            line.push('"');
        } else {
            line.push_str(&value);
        }
    }
    Ok(())
}

/// Appends one content line with folding and CRLF.
fn push_folded(line: &str, out: &mut String) {
    let mut budget = FOLD_AT;
    let mut used = 0;
    for c in line.chars() {
        let len = c.len_utf8();
        if used + len > budget {
            out.push_str("\r\n ");
            used = 0;
            // The leading space counts towards the 75 octets.
            budget = FOLD_AT - 1;
        }
        out.push(c);
        used += len;
    }
    out.push_str("\r\n");
}

/// Writes one property as a folded content line. The value must already be
/// in iCalendar syntax (escaped where needed).
pub fn write_property(property: &Property, out: &mut String) -> Result<(), IcalError> {
    check_name(&property.name)?;
    check_raw(&property.value)?;
    let mut line = String::with_capacity(property.name.len() + property.value.len() + 16);
    line.push_str(&property.name);
    for parameter in &property.parameters {
        write_parameter(parameter, &mut line)?;
    }
    line.push(':');
    line.push_str(&property.value);
    push_folded(&line, out);
    Ok(())
}

/// Serialises a component tree (iterative, so depth is not a concern).
pub fn write_component(component: &Component, out: &mut String) -> Result<(), IcalError> {
    enum Step<'a> {
        Open(&'a Component),
        Close(&'a str),
    }
    let mut steps = vec![Step::Open(component)];
    while let Some(step) = steps.pop() {
        match step {
            Step::Open(component) => {
                check_name(&component.name)?;
                push_folded(&format!("BEGIN:{}", component.name), out);
                for property in &component.properties {
                    write_property(property, out)?;
                }
                steps.push(Step::Close(&component.name));
                for child in component.components.iter().rev() {
                    steps.push(Step::Open(child));
                }
            }
            Step::Close(name) => push_folded(&format!("END:{name}"), out),
        }
    }
    Ok(())
}

/// Serialises a component tree into a new string.
pub fn to_string(component: &Component) -> Result<String, IcalError> {
    let mut out = String::new();
    write_component(component, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests;
