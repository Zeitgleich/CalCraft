//! jCal (RFC 7265) and jCard (RFC 7095): JSON forms of iCalendar and vCard.
//!
//! Output is written by hand (to keep property order and to stream large
//! exports); input is parsed with `serde_json` after a cheap pre-scan that
//! bounds nesting and the number of JSON values, so hostile documents
//! cannot exhaust memory.

use crate::{
    Component, IcalError,
    parser::{Limits, MAX_DEPTH},
    values::{
        Dialect, TypedProperty, TypedValue, from_typed, is_json_number, is_numeric_recur_part,
        to_typed,
    },
};

/// Deepest JSON nesting in a valid document: two levels per component plus
/// property, value and structured value arrays.
const MAX_JSON_DEPTH: usize = 2 * MAX_DEPTH + 6;

/// Appends `s` as a JSON string literal.
pub(crate) fn push_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // U+2028/2029 are escaped so the output is also valid JavaScript.
            c if c.is_control() || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_number_or_string(s: &str, out: &mut String) {
    if is_json_number(s) {
        out.push_str(s);
    } else {
        push_json_string(s, out);
    }
}

fn push_strings(values: &[String], out: &mut String) {
    if let [single] = values {
        push_json_string(single, out);
    } else {
        out.push('[');
        for (i, value) in values.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            push_json_string(value, out);
        }
        out.push(']');
    }
}

fn push_value(value: &TypedValue, out: &mut String) {
    match value {
        TypedValue::Text(text) => push_json_string(text, out),
        TypedValue::Number(n) => push_number_or_string(n, out),
        TypedValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        TypedValue::Structured(components) => {
            if let [single] = components.as_slice()
                && single.len() == 1
            {
                push_json_string(&single[0], out);
                return;
            }
            out.push('[');
            for (i, values) in components.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_strings(values, out);
            }
            out.push(']');
        }
        TypedValue::Recur(parts) => {
            out.push('{');
            for (i, (key, values)) in parts.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_json_string(key, out);
                out.push(':');
                let numeric = is_numeric_recur_part(key);
                let push_one = |v: &String, out: &mut String| {
                    if numeric {
                        push_number_or_string(v, out);
                    } else {
                        push_json_string(v, out);
                    }
                };
                if let [single] = values.as_slice() {
                    push_one(single, out);
                } else {
                    out.push('[');
                    for (j, value) in values.iter().enumerate() {
                        if j > 0 {
                            out.push(',');
                        }
                        push_one(value, out);
                    }
                    out.push(']');
                }
            }
            out.push('}');
        }
        TypedValue::Period(start, end) => {
            out.push('[');
            push_json_string(start, out);
            out.push(',');
            push_json_string(end, out);
            out.push(']');
        }
        TypedValue::Geo(lat, lon) => {
            out.push('[');
            push_number_or_string(lat, out);
            out.push(',');
            push_number_or_string(lon, out);
            out.push(']');
        }
    }
}

fn push_property(property: &TypedProperty, out: &mut String) {
    out.push('[');
    push_json_string(&property.name, out);
    out.push_str(",{");
    for (i, (name, values)) in property.params.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_json_string(name, out);
        out.push(':');
        push_strings(values, out);
    }
    out.push_str("},");
    push_json_string(&property.vtype, out);
    for value in &property.values {
        out.push(',');
        push_value(value, out);
    }
    out.push(']');
}

fn push_head(component: &Component, dialect: Dialect, out: &mut String) {
    out.push('[');
    push_json_string(&component.name.to_ascii_lowercase(), out);
    out.push_str(",[");
    for (i, property) in component.properties.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_property(&to_typed(property, dialect), out);
    }
    out.push(']');
}

/// Appends a jCal component's name and properties and opens its
/// sub-component array; the caller closes it with `]]`.
pub(crate) fn write_open(component: &Component, out: &mut String) {
    push_head(component, Dialect::ICal, out);
    out.push_str(",[");
}

/// Appends the jCal (`VCALENDAR` and children) or jCard (`VCARD`) form of a
/// component.
pub(crate) fn write_component(component: &Component, dialect: Dialect, out: &mut String) {
    push_head(component, dialect, out);
    // jCard has no sub-component array.
    if dialect == Dialect::ICal {
        out.push_str(",[");
        for (i, child) in component.components.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            write_component(child, dialect, out);
        }
        out.push(']');
    }
    out.push(']');
}

/// Bounds nesting depth and the number of values before parsing.
fn prescan(data: &[u8], max_values: usize) -> Result<(), IcalError> {
    let mut depth = 0usize;
    let mut values = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for &b in data {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return Err(IcalError::Limit("JSON nested too deeply"));
                }
                values += 1;
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            b',' | b':' => values += 1,
            _ => {}
        }
        if values > max_values {
            return Err(IcalError::Limit("too many JSON values"));
        }
    }
    Ok(())
}

/// A parsed JSON value. Unlike `serde_json::Value`, objects keep the
/// order of their members, so parameter order survives a round trip.
#[derive(Debug)]
enum Value {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    fn as_object(&self) -> Option<&[(String, Value)]> {
        match self {
            Self::Object(members) => Some(members),
            _ => None,
        }
    }

    fn is_string(&self) -> bool {
        matches!(self, Self::String(_))
    }
}

struct ValueVisitor;

impl<'de> serde::de::Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.to_string()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.to_string()))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
        Ok(Value::Number(
            serde_json::Number::from_f64(v).map_or_else(|| v.to_string(), |n| n.to_string()),
        ))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut members = Vec::new();
        while let Some(member) = map.next_entry::<String, Value>()? {
            members.push(member);
        }
        Ok(Value::Object(members))
    }
}

impl<'de> serde::Deserialize<'de> for Value {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ValueVisitor)
    }
}

struct Reader<'a> {
    dialect: Dialect,
    limits: &'a Limits,
    components: usize,
    properties: usize,
}

const MALFORMED: IcalError = IcalError::Malformed("invalid jCal/jCard structure");

fn as_str(value: &Value) -> Result<&str, IcalError> {
    value.as_str().ok_or(MALFORMED)
}

fn scalar_string(value: &Value) -> Result<String, IcalError> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(MALFORMED),
    }
}

fn strings(value: &Value) -> Result<Vec<String>, IcalError> {
    match value {
        Value::Array(items) => items.iter().map(scalar_string).collect(),
        other => Ok(vec![scalar_string(other)?]),
    }
}

impl Reader<'_> {
    fn value(&self, name: &str, vtype: &str, value: &Value) -> Result<TypedValue, IcalError> {
        Ok(match value {
            Value::String(s) if vtype == "period" => {
                let (start, end) = s.split_once('/').ok_or(MALFORMED)?;
                TypedValue::Period(start.to_owned(), end.to_owned())
            }
            Value::String(s) => TypedValue::Text(s.clone()),
            Value::Number(n) => TypedValue::Number(n.clone()),
            Value::Bool(b) => TypedValue::Bool(*b),
            Value::Array(items) if vtype == "period" => match items.as_slice() {
                [start, end] => TypedValue::Period(scalar_string(start)?, scalar_string(end)?),
                _ => return Err(MALFORMED),
            },
            Value::Array(items) if self.dialect == Dialect::ICal && name == "geo" => {
                match items.as_slice() {
                    [lat, lon] => TypedValue::Geo(scalar_string(lat)?, scalar_string(lon)?),
                    _ => return Err(MALFORMED),
                }
            }
            Value::Array(items) => {
                TypedValue::Structured(items.iter().map(strings).collect::<Result<_, _>>()?)
            }
            Value::Object(parts) if vtype == "recur" => TypedValue::Recur(
                parts
                    .iter()
                    .map(|(key, value)| Ok((key.to_ascii_lowercase(), strings(value)?)))
                    .collect::<Result<_, IcalError>>()?,
            ),
            _ => return Err(MALFORMED),
        })
    }

    fn property(&mut self, value: &Value) -> Result<crate::Property, IcalError> {
        self.properties += 1;
        if self.properties > self.limits.max_properties {
            return Err(IcalError::Limit("too many properties"));
        }
        let items = value.as_array().ok_or(MALFORMED)?;
        let [name, params, vtype, values @ ..] = items else {
            return Err(MALFORMED);
        };
        let name = as_str(name)?.to_ascii_lowercase();
        let vtype = as_str(vtype)?.to_ascii_lowercase();
        let params = params
            .as_object()
            .ok_or(MALFORMED)?
            .iter()
            .map(|(key, value)| Ok((key.to_ascii_lowercase(), strings(value)?)))
            .collect::<Result<Vec<_>, IcalError>>()?;
        let values = values
            .iter()
            .map(|v| self.value(&name, &vtype, v))
            .collect::<Result<Vec<_>, _>>()?;
        from_typed(
            &TypedProperty {
                name,
                params,
                vtype,
                values,
            },
            self.dialect,
        )
    }

    fn component(&mut self, value: &Value, depth: usize) -> Result<Component, IcalError> {
        if depth >= MAX_DEPTH {
            return Err(IcalError::Limit("components nested too deeply"));
        }
        self.components += 1;
        if self.components > self.limits.max_components {
            return Err(IcalError::Limit("too many components"));
        }
        let items = value.as_array().ok_or(MALFORMED)?;
        let (name, properties, children) = match items {
            [name, properties] => (name, properties, None),
            [name, properties, children] => (name, properties, Some(children)),
            _ => return Err(MALFORMED),
        };
        let name = as_str(name)?;
        if name.is_empty() || !crate::parser::is_name(name) || name.len() > 128 {
            return Err(IcalError::Malformed("invalid component name"));
        }
        let properties = properties
            .as_array()
            .ok_or(MALFORMED)?
            .iter()
            .map(|p| self.property(p))
            .collect::<Result<Vec<_>, _>>()?;
        let components = match children {
            Some(children) => children
                .as_array()
                .ok_or(MALFORMED)?
                .iter()
                .map(|c| self.component(c, depth + 1))
                .collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        Ok(Component {
            name: name.to_ascii_uppercase(),
            properties,
            components,
        })
    }
}

/// Parses a jCal/jCard document: a single component or an array of them.
pub(crate) fn read(
    data: &[u8],
    dialect: Dialect,
    limits: &Limits,
) -> Result<Vec<Component>, IcalError> {
    if data.len() > limits.max_size {
        return Err(IcalError::TooLarge);
    }
    // Every property needs a few values, so this bound is generous.
    prescan(data, limits.max_properties.saturating_mul(8))?;
    let document: Value =
        serde_json::from_slice(data).map_err(|_| IcalError::Malformed("invalid JSON"))?;
    let mut reader = Reader {
        dialect,
        limits,
        components: 0,
        properties: 0,
    };
    let items = document.as_array().ok_or(MALFORMED)?;
    if items.first().is_some_and(Value::is_string) {
        return Ok(vec![reader.component(&document, 0)?]);
    }
    items.iter().map(|c| reader.component(c, 0)).collect()
}

#[cfg(test)]
mod tests;
