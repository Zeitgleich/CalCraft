//! Content-line parser (RFC 5545 §3.1) producing a component tree.

use crate::{IcalError, MAX_OBJECT_SIZE, MAX_VCARD_SIZE};

pub(crate) const MAX_DEPTH: usize = 8;
const MAX_COMPONENTS: usize = 1_000;
const MAX_PROPERTIES: usize = 10_000;
pub(crate) const MAX_PARAMETERS: usize = 64;
pub(crate) const MAX_NAME_LEN: usize = 128;

/// A property parameter, e.g. `TZID=Europe/Berlin`. Values are unquoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    /// Uppercase parameter name.
    pub name: String,
    pub values: Vec<String>,
}

/// A property line. The value is kept verbatim (still escaped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    /// Uppercase property name.
    pub name: String,
    pub parameters: Vec<Parameter>,
    pub value: String,
}

impl Property {
    pub fn parameter(&self, name: &str) -> Option<&Parameter> {
        self.parameters.iter().find(|p| p.name == name)
    }
}

/// A component such as `VCALENDAR` or `VEVENT`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Component {
    /// Uppercase component name.
    pub name: String,
    pub properties: Vec<Property>,
    pub components: Vec<Component>,
}

impl Component {
    pub fn property(&self, name: &str) -> Option<&Property> {
        self.properties.iter().find(|p| p.name == name)
    }

    pub fn properties_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> {
        self.properties.iter().filter(move |p| p.name == name)
    }
}

/// Resource limits for one parsed stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub max_size: usize,
    pub max_components: usize,
    pub max_properties: usize,
}

/// Limits for a single stored object (PUT).
const OBJECT_LIMITS: Limits = Limits {
    max_size: MAX_OBJECT_SIZE,
    max_components: MAX_COMPONENTS,
    max_properties: MAX_PROPERTIES,
};

/// Limits for a single stored vCard, which may embed photos.
const VCARD_LIMITS: Limits = Limits {
    max_size: MAX_VCARD_SIZE,
    ..OBJECT_LIMITS
};

/// Parses an iCalendar stream with exactly one top-level component.
pub fn parse(data: &[u8]) -> Result<Component, IcalError> {
    let mut roots = parse_with(data, &OBJECT_LIMITS, false)?;
    roots.pop().ok_or(IcalError::Malformed("no component"))
}

/// Like [`parse`], but with the larger size limit of a vCard.
pub(crate) fn parse_vcard(data: &[u8]) -> Result<Component, IcalError> {
    let mut roots = parse_with(data, &VCARD_LIMITS, false)?;
    roots.pop().ok_or(IcalError::Malformed("no component"))
}

/// Parses a stream with any number of top-level components (an import
/// file: several `VCALENDAR`s or `VCARD`s), within `limits`.
pub(crate) fn parse_all(data: &[u8], limits: &Limits) -> Result<Vec<Component>, IcalError> {
    parse_with(data, limits, true)
}

fn parse_with(data: &[u8], limits: &Limits, many: bool) -> Result<Vec<Component>, IcalError> {
    if data.len() > limits.max_size {
        return Err(IcalError::TooLarge);
    }
    let text = std::str::from_utf8(data).map_err(|_| IcalError::Malformed("not UTF-8"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    let mut stack: Vec<Component> = Vec::new();
    let mut roots: Vec<Component> = Vec::new();
    let mut components = 0usize;
    let mut properties = 0usize;

    for line in unfold(text) {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        if !many && !roots.is_empty() {
            return Err(IcalError::Malformed(
                "content after the end of the calendar",
            ));
        }
        let property = parse_line(&line)?;
        match property.name.as_str() {
            "BEGIN" => {
                if stack.len() >= MAX_DEPTH {
                    return Err(IcalError::Limit("components nested too deeply"));
                }
                components += 1;
                if components > limits.max_components {
                    return Err(IcalError::Limit("too many components"));
                }
                stack.push(Component {
                    name: component_name(&property.value)?,
                    ..Component::default()
                });
            }
            "END" => {
                let name = component_name(&property.value)?;
                let component = stack
                    .pop()
                    .ok_or(IcalError::Malformed("END without BEGIN"))?;
                if component.name != name {
                    return Err(IcalError::Malformed("mismatched END"));
                }
                match stack.last_mut() {
                    Some(parent) => parent.components.push(component),
                    None => roots.push(component),
                }
            }
            _ => {
                properties += 1;
                if properties > limits.max_properties {
                    return Err(IcalError::Limit("too many properties"));
                }
                stack
                    .last_mut()
                    .ok_or(IcalError::Malformed("property outside of a component"))?
                    .properties
                    .push(property);
            }
        }
    }
    if !stack.is_empty() {
        return Err(IcalError::Malformed("unterminated component"));
    }
    if roots.is_empty() {
        return Err(IcalError::Malformed("no component"));
    }
    Ok(roots)
}

/// Splits into logical lines, joining folded continuation lines.
fn unfold(text: &str) -> impl Iterator<Item = Result<String, IcalError>> + '_ {
    let mut lines = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .peekable();
    std::iter::from_fn(move || {
        let first = lines.next()?;
        let mut line = first.to_owned();
        while let Some(next) = lines.peek() {
            match next.strip_prefix([' ', '\t']) {
                Some(rest) => {
                    line.push_str(rest);
                    lines.next();
                }
                None => break,
            }
        }
        // Control characters other than HTAB are not allowed (RFC 5545 §3.1).
        if line.chars().any(|c| c.is_control() && c != '\t') {
            return Some(Err(IcalError::Malformed(
                "control character in content line",
            )));
        }
        Some(Ok(line))
    })
}

fn component_name(value: &str) -> Result<String, IcalError> {
    if value.is_empty() || value.len() > MAX_NAME_LEN || !is_name(value) {
        return Err(IcalError::Malformed("invalid component name"));
    }
    Ok(value.to_ascii_uppercase())
}

pub(crate) fn is_name(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Parses `name *(";" param) ":" value`.
fn parse_line(line: &str) -> Result<Property, IcalError> {
    let name_end = line
        .find([';', ':'])
        .ok_or(IcalError::Malformed("content line without ':'"))?;
    let name = &line[..name_end];
    if name.is_empty() || name.len() > MAX_NAME_LEN || !is_name(name) {
        return Err(IcalError::Malformed("invalid property name"));
    }

    let mut parameters = Vec::new();
    let mut rest = &line[name_end..];
    while let Some(after) = rest.strip_prefix(';') {
        if parameters.len() >= MAX_PARAMETERS {
            return Err(IcalError::Limit("too many parameters"));
        }
        let eq = after
            .find('=')
            .ok_or(IcalError::Malformed("parameter without '='"))?;
        let param_name = &after[..eq];
        if param_name.is_empty() || param_name.len() > MAX_NAME_LEN || !is_name(param_name) {
            return Err(IcalError::Malformed("invalid parameter name"));
        }
        let (values, remaining) = parse_param_values(&after[eq + 1..])?;
        parameters.push(Parameter {
            name: param_name.to_ascii_uppercase(),
            values,
        });
        rest = remaining;
    }
    let value = rest
        .strip_prefix(':')
        .ok_or(IcalError::Malformed("content line without ':'"))?;

    Ok(Property {
        name: name.to_ascii_uppercase(),
        parameters,
        value: value.to_owned(),
    })
}

/// Parses `param-value *("," param-value)` and returns the remaining input.
fn parse_param_values(mut input: &str) -> Result<(Vec<String>, &str), IcalError> {
    let mut values = Vec::new();
    loop {
        if values.len() >= MAX_PARAMETERS {
            return Err(IcalError::Limit("too many parameter values"));
        }
        if let Some(quoted) = input.strip_prefix('"') {
            let end = quoted
                .find('"')
                .ok_or(IcalError::Malformed("unterminated quoted parameter"))?;
            values.push(quoted[..end].to_owned());
            input = &quoted[end + 1..];
        } else {
            let end = input.find([',', ';', ':']).unwrap_or(input.len());
            values.push(input[..end].to_owned());
            input = &input[end..];
        }
        match input.strip_prefix(',') {
            Some(next) => input = next,
            None => return Ok((values, input)),
        }
    }
}

#[cfg(test)]
mod tests;
