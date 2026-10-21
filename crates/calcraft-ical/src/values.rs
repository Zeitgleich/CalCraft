//! Typed property values as used by jCal (RFC 7265), jCard (RFC 7095) and
//! xCal (RFC 6321).
//!
//! The content-line parser keeps values verbatim. These alternative formats
//! need the value type of each property, TEXT unescaped, lists split and
//! dates in the extended ISO 8601 form. [`to_typed`] and [`from_typed`]
//! convert in both directions; values that do not have the expected syntax
//! are passed through as type `unknown`, so no data is lost.

use crate::{
    IcalError, Parameter, Property,
    parser::{MAX_NAME_LEN, is_name},
    write::{escape_text, split_unescaped, unescape_text},
};

/// Which property table applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    ICal,
    VCard,
}

/// One value of a property.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TypedValue {
    /// TEXT (unescaped) or any other type as a string (raw, or for dates
    /// and times in extended format).
    Text(String),
    /// INTEGER or FLOAT literal.
    Number(String),
    Bool(bool),
    /// Structured TEXT (`N`, `ADR`, `ORG`, `REQUEST-STATUS`, …): components,
    /// each with one or more unescaped values.
    Structured(Vec<Vec<String>>),
    /// RECUR as lowercase rule parts.
    Recur(Vec<(String, Vec<String>)>),
    /// PERIOD as start and end or duration.
    Period(String, String),
    /// iCalendar GEO as latitude and longitude.
    Geo(String, String),
}

/// A property in the typed model, names in lowercase.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TypedProperty {
    pub name: String,
    pub params: Vec<(String, Vec<String>)>,
    pub vtype: String,
    pub values: Vec<TypedValue>,
}

/// Default value type of a property (lowercase), `unknown` if not known.
pub(crate) fn default_type(dialect: Dialect, name: &str) -> &'static str {
    match dialect {
        Dialect::ICal => match name {
            "CALSCALE" | "METHOD" | "PRODID" | "VERSION" | "CATEGORIES" | "CLASS" | "COMMENT"
            | "DESCRIPTION" | "LOCATION" | "RESOURCES" | "STATUS" | "SUMMARY" | "TRANSP"
            | "TZID" | "TZNAME" | "CONTACT" | "RELATED-TO" | "UID" | "ACTION"
            | "REQUEST-STATUS" | "NAME" | "COLOR" | "X-WR-CALNAME" | "X-WR-CALDESC" => "text",
            "ATTACH" | "TZURL" | "URL" | "IMAGE" | "CONFERENCE" | "SOURCE" => "uri",
            "ATTENDEE" | "ORGANIZER" => "cal-address",
            "DTSTART" | "DTEND" | "DUE" | "RECURRENCE-ID" | "EXDATE" | "RDATE" | "COMPLETED"
            | "CREATED" | "DTSTAMP" | "LAST-MODIFIED" => "date-time",
            "DURATION" | "TRIGGER" | "REFRESH-INTERVAL" => "duration",
            "PERCENT-COMPLETE" | "PRIORITY" | "REPEAT" | "SEQUENCE" => "integer",
            "GEO" => "float",
            "TZOFFSETFROM" | "TZOFFSETTO" => "utc-offset",
            "FREEBUSY" => "period",
            "RRULE" | "EXRULE" => "recur",
            _ => "unknown",
        },
        Dialect::VCard => match name {
            "FN" | "N" | "NICKNAME" | "ORG" | "TITLE" | "ROLE" | "NOTE" | "CATEGORIES"
            | "PRODID" | "KIND" | "GENDER" | "ADR" | "EMAIL" | "LABEL" | "MAILER" | "CLASS"
            | "SORT-STRING" | "VERSION" | "TEL" | "XML" | "UID" | "TZ" | "CLIENTPIDMAP"
            | "NAME" | "PROFILE" | "AGENT" => "text",
            "SOURCE" | "PHOTO" | "LOGO" | "SOUND" | "URL" | "KEY" | "IMPP" | "MEMBER"
            | "RELATED" | "FBURL" | "CALADRURI" | "CALURI" | "GEO" => "uri",
            "BDAY" | "ANNIVERSARY" => "date-and-or-time",
            "REV" => "timestamp",
            "LANG" => "language-tag",
            _ => "unknown",
        },
    }
}

/// Properties whose value is a comma-separated list.
fn is_list(dialect: Dialect, name: &str) -> bool {
    match dialect {
        Dialect::ICal => matches!(
            name,
            "CATEGORIES" | "RESOURCES" | "EXDATE" | "RDATE" | "FREEBUSY"
        ),
        Dialect::VCard => matches!(name, "NICKNAME" | "CATEGORIES"),
    }
}

/// Properties with a structured (`;`-separated) TEXT value.
fn is_structured(dialect: Dialect, name: &str) -> bool {
    match dialect {
        Dialect::ICal => name == "REQUEST-STATUS",
        Dialect::VCard => matches!(name, "N" | "ADR" | "ORG" | "GENDER" | "CLIENTPIDMAP"),
    }
}

/// Date and time types whose representation differs between the formats.
fn is_temporal(vtype: &str) -> bool {
    matches!(
        vtype,
        "date" | "date-time" | "time" | "date-and-or-time" | "timestamp" | "utc-offset"
    )
}

/// The value type the content line implies when there is no `VALUE`.
fn implied_type(dialect: Dialect, name: &str, value: &str) -> &'static str {
    let default = default_type(dialect, name);
    if dialect == Dialect::ICal {
        let first = value.split(',').next().unwrap_or_default();
        if default == "date-time" {
            if name == "RDATE" && first.contains('/') {
                return "period";
            }
            if first.len() == 8 && first.bytes().all(|b| b.is_ascii_digit()) {
                return "date";
            }
        }
        if name == "TRIGGER" && first.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
            return "date-time";
        }
    }
    default
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `20261021` → `2026-10-21`.
fn ext_date(s: &str) -> Option<String> {
    (s.len() == 8 && all_digits(s)).then(|| format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..]))
}

/// `100000Z` → `10:00:00Z`.
fn ext_time(s: &str) -> Option<String> {
    let (body, zone) = match s.strip_suffix('Z') {
        Some(body) => (body, "Z"),
        None => (s, ""),
    };
    (body.len() == 6 && all_digits(body))
        .then(|| format!("{}:{}:{}{zone}", &body[..2], &body[2..4], &body[4..]))
}

/// `20261021T100000Z` → `2026-10-21T10:00:00Z`.
fn ext_date_time(s: &str) -> Option<String> {
    let (date, time) = s.split_once('T')?;
    Some(format!("{}T{}", ext_date(date)?, ext_time(time)?))
}

/// `+0130` → `+01:30`, `-053015` → `-05:30:15`, `-05` stays.
fn ext_offset(s: &str) -> Option<String> {
    let sign = s.get(..1).filter(|c| *c == "+" || *c == "-")?;
    let digits = &s[1..];
    if !all_digits(digits) || !matches!(digits.len(), 2 | 4 | 6) {
        return None;
    }
    let groups: Vec<&str> = (0..digits.len() / 2)
        .map(|i| &digits[i * 2..i * 2 + 2])
        .collect();
    Some(format!("{sign}{}", groups.join(":")))
}

/// vCard DATE-AND-OR-TIME (RFC 6350 §4.3) in basic format to extended
/// format, e.g. `19850412`, `--0412`, `T102200Z`, `19961022T140000-0500`.
fn ext_vcard(s: &str) -> Option<String> {
    if !s
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'T' | b'Z'))
    {
        return None;
    }
    let (date, time) = match s.split_once('T') {
        Some((date, time)) => (date, Some(time)),
        None => (s, None),
    };
    let date = match date.len() {
        0 | 4 if all_digits(date) || date.is_empty() => date.to_owned(),
        8 => ext_date(date)?,
        // --MMDD, --MM, ---DD
        _ if date.starts_with("--") => {
            let rest = date.trim_start_matches('-');
            let dashes = &date[..date.len() - rest.len()];
            if !all_digits(rest) {
                return None;
            }
            match rest.len() {
                4 => format!("{dashes}{}-{}", &rest[..2], &rest[2..]),
                2 => date.to_owned(),
                _ => return None,
            }
        }
        _ => return None,
    };
    let Some(time) = time else {
        return Some(date);
    };
    let (time, zone) = if let Some(body) = time.strip_suffix('Z') {
        (body, "Z".to_owned())
    } else if let Some(at) = time.rfind(['+', '-']).filter(|&at| at > 0) {
        (&time[..at], ext_offset(&time[at..])?)
    } else {
        (time, String::new())
    };
    if !all_digits(time) || !matches!(time.len(), 2 | 4 | 6) {
        return None;
    }
    let groups: Vec<&str> = (0..time.len() / 2)
        .map(|i| &time[i * 2..i * 2 + 2])
        .collect();
    Some(format!("{date}T{}{zone}", groups.join(":")))
}

/// Extended date/time/offset to the basic form used in content lines:
/// separators are removed, leading truncation dashes and zone signs kept.
fn compact(s: &str) -> String {
    let (date, time) = match s.split_once('T') {
        Some((date, time)) => (date, Some(time)),
        None => (s, None),
    };
    let mut out = String::with_capacity(s.len());
    let rest = date.trim_start_matches(['-', '+']);
    let lead = &date[..date.len() - rest.len()];
    out.push_str(lead);
    // An offset (`+01:00`) has no date dashes; a date has no colons.
    out.extend(rest.chars().filter(|c| *c != '-' && *c != ':'));
    if let Some(time) = time {
        out.push('T');
        out.extend(time.chars().filter(|c| *c != ':'));
    }
    out
}

fn is_number(s: &str) -> bool {
    let s = s.strip_prefix(['-', '+']).unwrap_or(s);
    let (int, frac) = match s.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (s, None),
    };
    all_digits(int) && frac.is_none_or(all_digits)
}

/// Whether `s` can be written as a JSON number literal.
pub(crate) fn is_json_number(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    is_number(body)
        && !body.starts_with('+')
        && !(body.len() > 1 && body.starts_with('0') && !body.starts_with("0."))
}

fn ext_until(s: &str) -> Option<String> {
    if s.len() == 8 {
        ext_date(s)
    } else {
        ext_date_time(s)
    }
}

fn parse_recur(value: &str) -> Option<TypedValue> {
    let mut parts = Vec::new();
    for part in value.split(';') {
        let (key, value) = part.split_once('=')?;
        let key = key.to_ascii_lowercase();
        // Unknown rule parts make the whole value `unknown` (kept raw).
        if !is_recur_part(&key) {
            return None;
        }
        let values = if key == "until" {
            vec![ext_until(value)?]
        } else {
            let values: Vec<String> = value.split(',').map(str::to_owned).collect();
            if !values.iter().all(|v| is_recur_value(v)) {
                return None;
            }
            values
        };
        parts.push((key, values));
    }
    Some(TypedValue::Recur(parts))
}

/// Rule parts of RFC 5545 §3.3.10 and RFC 7529 (RSCALE, SKIP).
fn is_recur_part(key: &str) -> bool {
    matches!(
        key,
        "freq"
            | "until"
            | "count"
            | "interval"
            | "bysecond"
            | "byminute"
            | "byhour"
            | "byday"
            | "bymonthday"
            | "byyearday"
            | "byweekno"
            | "bymonth"
            | "bysetpos"
            | "wkst"
            | "rscale"
            | "skip"
    )
}

/// Characters that can occur in a rule part value (`MO`, `-1SU`, `10`,
/// `GREGORIAN`, `5L`).
fn is_recur_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-')
}

/// Characters of a date, time, offset or duration in basic form.
fn is_temporal_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|b| {
            b.is_ascii_digit()
                || matches!(
                    b,
                    b'T' | b'Z' | b'+' | b'-' | b'P' | b'W' | b'D' | b'H' | b'M' | b'S'
                )
        })
}

/// Rule parts that are numbers in jCal.
pub(crate) fn is_numeric_recur_part(key: &str) -> bool {
    matches!(
        key,
        "count"
            | "interval"
            | "bysecond"
            | "byminute"
            | "byhour"
            | "bymonthday"
            | "byyearday"
            | "byweekno"
            | "bymonth"
            | "bysetpos"
    )
}

/// Canonical order of rule parts in RFC 5545 (FREQ first).
fn recur_rank(key: &str) -> usize {
    const ORDER: [&str; 14] = [
        "freq",
        "until",
        "count",
        "interval",
        "bysecond",
        "byminute",
        "byhour",
        "byday",
        "bymonthday",
        "byyearday",
        "byweekno",
        "bymonth",
        "bysetpos",
        "wkst",
    ];
    ORDER.iter().position(|k| *k == key).unwrap_or(ORDER.len())
}

fn period_end(s: &str) -> bool {
    s.starts_with(['P', '+', '-'])
}

fn convert_value(dialect: Dialect, name: &str, vtype: &str, part: &str) -> Option<TypedValue> {
    Some(match (dialect, vtype) {
        (_, "text") if is_structured(dialect, name) => {
            let components = split_unescaped(part, ';')
                .into_iter()
                .map(|component| match dialect {
                    // vCard components can themselves be lists.
                    Dialect::VCard => split_unescaped(component, ',')
                        .into_iter()
                        .map(unescape_text)
                        .collect(),
                    Dialect::ICal => vec![unescape_text(component)],
                })
                .collect();
            TypedValue::Structured(components)
        }
        (_, "text") => TypedValue::Text(unescape_text(part)),
        (Dialect::ICal, "date") => TypedValue::Text(ext_date(part)?),
        (Dialect::ICal, "date-time") => TypedValue::Text(ext_date_time(part)?),
        (Dialect::ICal, "time") => TypedValue::Text(ext_time(part)?),
        (Dialect::VCard, t) if is_temporal(t) && t != "utc-offset" => {
            TypedValue::Text(ext_vcard(part)?)
        }
        (_, "utc-offset") => TypedValue::Text(ext_offset(part)?),
        (Dialect::ICal, "period") => {
            let (start, end) = part.split_once('/')?;
            let end = if period_end(end) {
                end.to_owned()
            } else {
                ext_date_time(end)?
            };
            TypedValue::Period(ext_date_time(start)?, end)
        }
        (Dialect::ICal, "float") if name == "GEO" => {
            let (lat, lon) = part.split_once(';')?;
            if !is_number(lat) || !is_number(lon) {
                return None;
            }
            TypedValue::Geo(lat.to_owned(), lon.to_owned())
        }
        (_, "integer" | "float") => {
            if !is_number(part) {
                return None;
            }
            TypedValue::Number(part.strip_prefix('+').unwrap_or(part).to_owned())
        }
        (_, "boolean") => match part.to_ascii_uppercase().as_str() {
            "TRUE" => TypedValue::Bool(true),
            "FALSE" => TypedValue::Bool(false),
            _ => return None,
        },
        (Dialect::ICal, "recur") => parse_recur(part)?,
        // URI, CAL-ADDRESS, DURATION, BINARY, unknown and extension types
        // are passed through verbatim.
        _ => TypedValue::Text(part.to_owned()),
    })
}

/// Converts a parsed property into the typed model.
pub(crate) fn to_typed(property: &Property, dialect: Dialect) -> TypedProperty {
    let mut params = Vec::new();
    let mut explicit = None;
    // Position and original values of VALUE, to restore it if needed.
    let mut original = None;
    for parameter in &property.parameters {
        if parameter.name == "VALUE" && explicit.is_none() {
            explicit = parameter.values.first().map(|v| v.to_ascii_lowercase());
            original = Some((params.len(), parameter.values.clone()));
        } else {
            params.push((
                parameter.name.to_ascii_lowercase(),
                parameter.values.clone(),
            ));
        }
    }
    let vtype = explicit
        .clone()
        .unwrap_or_else(|| implied_type(dialect, &property.name, &property.value).to_owned());
    let parts = if is_list(dialect, &property.name) {
        split_unescaped(&property.value, ',')
    } else {
        vec![property.value.as_str()]
    };
    let values: Option<Vec<TypedValue>> = parts
        .into_iter()
        .map(|part| convert_value(dialect, &property.name, &vtype, part))
        .collect();
    match values {
        Some(values) => TypedProperty {
            name: property.name.to_ascii_lowercase(),
            params,
            vtype,
            values,
        },
        None => {
            // Keep an explicit VALUE as a parameter, so it survives.
            if let Some((at, values)) = original {
                params.insert(at, ("value".to_owned(), values));
            }
            TypedProperty {
                name: property.name.to_ascii_lowercase(),
                params,
                vtype: "unknown".to_owned(),
                values: vec![TypedValue::Text(property.value.clone())],
            }
        }
    }
}

fn checked_name(name: &str) -> Result<String, IcalError> {
    if name.is_empty() || name.len() > MAX_NAME_LEN || !is_name(name) {
        return Err(IcalError::Invalid("invalid property or parameter name"));
    }
    Ok(name.to_ascii_uppercase())
}

const INVALID_VALUE: IcalError = IcalError::Invalid("value does not match its type");

fn recur_to_string(parts: &[(String, Vec<String>)]) -> Result<String, IcalError> {
    let mut parts: Vec<&(String, Vec<String>)> = parts.iter().collect();
    parts.sort_by_key(|(key, _)| recur_rank(key));
    let mut out = Vec::with_capacity(parts.len());
    for (key, values) in parts {
        let key = key.to_ascii_lowercase();
        if !is_recur_part(&key) || values.is_empty() {
            return Err(IcalError::Invalid("invalid RRULE part"));
        }
        let values: Vec<String> = if key == "until" {
            values.iter().map(|v| compact(v)).collect()
        } else {
            values.clone()
        };
        let valid = if key == "until" {
            values.len() == 1 && values.iter().all(|v| is_temporal_value(v))
        } else {
            values.iter().all(|v| is_recur_value(v))
        };
        if !valid {
            return Err(IcalError::Invalid("invalid RRULE value"));
        }
        out.push(format!("{}={}", key.to_ascii_uppercase(), values.join(",")));
    }
    Ok(out.join(";"))
}

fn temporal(value: &str) -> Result<String, IcalError> {
    checked_temporal(compact(value))
}

/// The end of a PERIOD: a date-time or a duration.
fn period_end_value(value: &str) -> Result<String, IcalError> {
    if period_end(value) {
        checked_temporal(value.to_owned())
    } else {
        temporal(value)
    }
}

fn checked_temporal(value: String) -> Result<String, IcalError> {
    if is_temporal_value(&value) {
        Ok(value)
    } else {
        Err(INVALID_VALUE)
    }
}

fn number(value: &str) -> Result<String, IcalError> {
    if is_number(value) && value.len() <= 64 {
        Ok(value.to_owned())
    } else {
        Err(INVALID_VALUE)
    }
}

fn value_to_string(vtype: &str, value: &TypedValue) -> Result<String, IcalError> {
    Ok(match value {
        TypedValue::Text(text) if vtype == "text" => escape_text(text),
        TypedValue::Text(text) if is_temporal(vtype) => temporal(text)?,
        TypedValue::Number(text) => number(text)?,
        TypedValue::Text(text) => text.clone(),
        TypedValue::Bool(true) => "TRUE".to_owned(),
        TypedValue::Bool(false) => "FALSE".to_owned(),
        TypedValue::Structured(components) => components
            .iter()
            .map(|values| {
                values
                    .iter()
                    .map(|v| escape_text(v))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect::<Vec<_>>()
            .join(";"),
        TypedValue::Recur(parts) => recur_to_string(parts)?,
        TypedValue::Period(start, end) => {
            format!("{}/{}", temporal(start)?, period_end_value(end)?)
        }
        TypedValue::Geo(lat, lon) => format!("{};{}", number(lat)?, number(lon)?),
    })
}

/// Converts a typed property back into a content line property.
pub(crate) fn from_typed(
    property: &TypedProperty,
    dialect: Dialect,
) -> Result<Property, IcalError> {
    let name = checked_name(&property.name)?;
    let vtype = property.vtype.to_ascii_lowercase();
    if property.values.is_empty() {
        return Err(IcalError::Invalid("property without value"));
    }
    let mut parameters = Vec::with_capacity(property.params.len() + 1);
    let has_value_param = property
        .params
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case("value"));
    if vtype != "unknown" && vtype != default_type(dialect, &name) && !has_value_param {
        let value = checked_name(&vtype)?;
        // vCard examples use lowercase value types, iCalendar uppercase.
        let value = match dialect {
            Dialect::ICal => value,
            Dialect::VCard => value.to_ascii_lowercase(),
        };
        parameters.push(Parameter {
            name: "VALUE".to_owned(),
            values: vec![value],
        });
    }
    for (param, values) in &property.params {
        parameters.push(Parameter {
            name: checked_name(param)?,
            values: values.clone(),
        });
    }
    let value = property
        .values
        .iter()
        .map(|v| value_to_string(&vtype, v))
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    Ok(Property {
        name,
        parameters,
        value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prop(line: &str) -> Property {
        let data = format!("BEGIN:X\r\n{line}\r\nEND:X\r\n");
        crate::parse(data.as_bytes()).unwrap().properties.remove(0)
    }

    fn round_trip(dialect: Dialect, line: &str) -> TypedProperty {
        let original = prop(line);
        let typed = to_typed(&original, dialect);
        let back = from_typed(&typed, dialect).unwrap();
        assert_eq!(back, original, "{line}");
        typed
    }

    #[test]
    fn ical_types() {
        let t = round_trip(Dialect::ICal, "DTSTART;TZID=Europe/Berlin:20261021T100000");
        assert_eq!(t.vtype, "date-time");
        assert_eq!(t.values, [TypedValue::Text("2026-10-21T10:00:00".into())]);
        let t = round_trip(Dialect::ICal, "DTSTART;VALUE=DATE:20261021");
        assert_eq!(t.vtype, "date");
        assert!(t.params.is_empty());
        let t = round_trip(Dialect::ICal, "SUMMARY:a\\, b\\; c\\nd\\\\");
        assert_eq!(t.values, [TypedValue::Text("a, b; c\nd\\".into())]);
        let t = round_trip(Dialect::ICal, "CATEGORIES:x\\,y,z");
        assert_eq!(t.values.len(), 2);
        let t = round_trip(
            Dialect::ICal,
            "RRULE:FREQ=WEEKLY;UNTIL=20261231T000000Z;BYDAY=MO,WE",
        );
        assert_eq!(
            t.values,
            [TypedValue::Recur(vec![
                ("freq".into(), vec!["WEEKLY".into()]),
                ("until".into(), vec!["2026-12-31T00:00:00Z".into()]),
                ("byday".into(), vec!["MO".into(), "WE".into()]),
            ])]
        );
        round_trip(Dialect::ICal, "EXDATE:20261028T100000Z,20261104T100000Z");
        round_trip(Dialect::ICal, "RDATE;VALUE=PERIOD:20261021T100000Z/PT1H");
        round_trip(Dialect::ICal, "TZOFFSETFROM:+0200");
        round_trip(Dialect::ICal, "GEO:37.386013;-122.082932");
        round_trip(Dialect::ICal, "PRIORITY:5");
        round_trip(Dialect::ICal, "TRIGGER:-PT15M");
        round_trip(Dialect::ICal, "TRIGGER;VALUE=DATE-TIME:20261021T090000Z");
        round_trip(Dialect::ICal, "REQUEST-STATUS:2.0;Success");
        round_trip(Dialect::ICal, "X-CUSTOM;X-P=1:raw\\nvalue");
        // A value that does not match its type survives as unknown.
        let t = round_trip(Dialect::ICal, "DTSTART:garbage");
        assert_eq!(t.vtype, "unknown");
        let t = round_trip(Dialect::ICal, "DTSTART;VALUE=DATE:garbage");
        assert_eq!(t.vtype, "unknown");
    }

    #[test]
    fn vcard_types() {
        let t = round_trip(Dialect::VCard, "N:Doe;John;Q.,R.;Dr.;");
        assert_eq!(
            t.values,
            [TypedValue::Structured(vec![
                vec!["Doe".into()],
                vec!["John".into()],
                vec!["Q.".into(), "R.".into()],
                vec!["Dr.".into()],
                vec![String::new()],
            ])]
        );
        let t = round_trip(Dialect::VCard, "BDAY:19850412");
        assert_eq!(t.values, [TypedValue::Text("1985-04-12".into())]);
        let t = round_trip(Dialect::VCard, "BDAY:--0412");
        assert_eq!(t.values, [TypedValue::Text("--04-12".into())]);
        let t = round_trip(Dialect::VCard, "REV:19951031T222710Z");
        assert_eq!(t.values, [TypedValue::Text("1995-10-31T22:27:10Z".into())]);
        round_trip(Dialect::VCard, "ANNIVERSARY:19960415T140000-0500");
        round_trip(
            Dialect::VCard,
            "TEL;VALUE=uri;TYPE=work:tel:+1-555-555-5555",
        );
        round_trip(Dialect::VCard, "ORG:ABC\\, Inc.;North American Division");
        round_trip(Dialect::VCard, "NICKNAME:Jim,Jimmie");
    }

    #[test]
    fn rejects_bad_names_and_injection() {
        let typed = TypedProperty {
            name: "summary\r\nx".into(),
            params: vec![],
            vtype: "text".into(),
            values: vec![TypedValue::Text("x".into())],
        };
        assert!(from_typed(&typed, Dialect::ICal).is_err());
        // TEXT newlines are escaped, so they cannot start a new line.
        let typed = TypedProperty {
            name: "summary".into(),
            params: vec![],
            vtype: "text".into(),
            values: vec![TypedValue::Text("x\r\nBEGIN:VEVENT".into())],
        };
        assert_eq!(
            from_typed(&typed, Dialect::ICal).unwrap().value,
            "x\\nBEGIN:VEVENT"
        );
    }

    #[test]
    fn rejects_values_that_do_not_match_their_type() {
        let bad = |vtype: &str, value: TypedValue| {
            let typed = TypedProperty {
                name: "x-test".into(),
                params: vec![],
                vtype: vtype.into(),
                values: vec![value],
            };
            from_typed(&typed, Dialect::ICal).is_err()
        };
        let recur = |key: &str, value: &str| {
            TypedValue::Recur(vec![
                ("freq".into(), vec!["DAILY".into()]),
                (key.into(), vec![value.into()]),
            ])
        };
        assert!(bad("recur", recur("x-evil", "1")));
        assert!(bad("recur", recur("count", "1;BYDAY=MO")));
        assert!(bad("recur", recur("byday", "MO:x")));
        assert!(bad("recur", recur("until", "2026-10-21;X=1")));
        assert!(!bad("recur", recur("byday", "-1SU")));
        assert!(bad("integer", TypedValue::Number("5;X".into())));
        assert!(bad("float", TypedValue::Geo("1.5".into(), "2;3".into())));
        assert!(bad(
            "date-time",
            TypedValue::Text("2026-10-21T10:00:00;x".into())
        ));
        assert!(bad(
            "period",
            TypedValue::Period("20261021T100000Z".into(), "P1D,x".into())
        ));
        // Unknown rule parts in stored data survive as an `unknown` value.
        let t = round_trip(Dialect::ICal, "RRULE:FREQ=DAILY;X-NAME=1");
        assert_eq!(t.vtype, "unknown");
    }

    #[test]
    fn json_numbers() {
        for ok in ["0", "5", "-1", "0.5", "37.386013"] {
            assert!(is_json_number(ok), "{ok}");
        }
        for bad in ["", "+1", "01", "1.", ".5", "1e5", "x"] {
            assert!(!is_json_number(bad), "{bad}");
        }
    }
}
