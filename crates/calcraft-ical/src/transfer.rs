//! Import and export of whole calendars and address books.
//!
//! **Import** turns a file in any supported format into individual objects
//! in iCalendar/vCard text: events, to-dos and journals are grouped by UID
//! (so recurrence overrides stay with their master), and the `VTIMEZONE`s
//! they reference are copied into each object. The caller must validate
//! every object exactly like a DAV PUT (`CalendarObject::parse` /
//! `VCard::parse`); nothing here bypasses those checks.
//!
//! **Export** works object by object ([`Exporter`]), so callers can stream
//! large collections without holding the whole file in memory.

use std::collections::{HashMap, HashSet};

use crate::{
    Component, IcalError, MAX_OBJECT_SIZE, Parameter, Property, csv,
    datetime::{DateTime, parse_date_or_datetime, parse_duration},
    jcal, parse,
    parser::{Limits, parse_all},
    values::Dialect,
    write::{escape_text, split_unescaped, to_string, unescape_text, write_property},
    xcal,
};

/// Largest accepted import file in bytes.
pub const MAX_IMPORT_SIZE: usize = 20 * 1024 * 1024;
/// Maximum number of objects in one import.
pub const MAX_IMPORT_OBJECTS: usize = 10_000;

/// Limits for parsing a whole import file. They bound memory use
/// (roughly 100–200 bytes per property in the parsed tree).
const IMPORT_LIMITS: Limits = Limits {
    max_size: MAX_IMPORT_SIZE,
    max_components: 20 * MAX_IMPORT_OBJECTS,
    max_properties: 50 * MAX_IMPORT_OBJECTS,
};

const PRODID: &str = concat!("-//CalCraft//CalCraft ", env!("CARGO_PKG_VERSION"), "//EN");

/// What kind of collection is imported into or exported from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Calendar,
    Addressbook,
}

/// File formats for import and export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// iCalendar `.ics` (RFC 5545).
    ICalendar,
    /// jCal (RFC 7265).
    JCal,
    /// xCal (RFC 6321).
    XCal,
    /// vCard `.vcf` (RFC 6350 / RFC 2426).
    VCard,
    /// jCard (RFC 7095).
    JCard,
    /// Comma-separated values with a header row.
    Csv,
}

impl Format {
    /// Parses the name used in URLs and on the command line.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "ics" | "ical" | "icalendar" => Self::ICalendar,
            "jcal" => Self::JCal,
            "xcal" => Self::XCal,
            "vcf" | "vcard" => Self::VCard,
            "jcard" => Self::JCard,
            "csv" => Self::Csv,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::ICalendar => "ics",
            Self::JCal => "jcal",
            Self::XCal => "xcal",
            Self::VCard => "vcf",
            Self::JCard => "jcard",
            Self::Csv => "csv",
        }
    }

    /// File name extension for downloads.
    pub fn extension(self) -> &'static str {
        match self {
            Self::ICalendar => "ics",
            Self::JCal | Self::JCard => "json",
            Self::XCal => "xml",
            Self::VCard => "vcf",
            Self::Csv => "csv",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::ICalendar => "text/calendar; charset=utf-8",
            Self::JCal => "application/calendar+json",
            Self::XCal => "application/calendar+xml; charset=utf-8",
            Self::VCard => "text/vcard; charset=utf-8",
            Self::JCard => "application/vcard+json",
            Self::Csv => "text/csv; charset=utf-8",
        }
    }

    /// The default format of a collection kind.
    pub fn default_for(target: Target) -> Self {
        match target {
            Target::Calendar => Self::ICalendar,
            Target::Addressbook => Self::VCard,
        }
    }

    /// Whether the format can hold this kind of collection.
    pub fn supports(self, target: Target) -> bool {
        match self {
            Self::ICalendar | Self::JCal | Self::XCal => target == Target::Calendar,
            Self::VCard | Self::JCard => target == Target::Addressbook,
            Self::Csv => true,
        }
    }
}

/// One object of an import: its UID (if known) and its iCalendar/vCard
/// text, or why it could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedObject {
    pub uid: Option<String>,
    pub data: Result<Vec<u8>, IcalError>,
}

/// Values the caller provides, so this crate stays free of clocks and
/// random numbers.
pub struct ImportContext<'a> {
    /// Current time as UTC DATE-TIME (`20261021T100000Z`), for `DTSTAMP`.
    pub dtstamp: &'a str,
    /// Creates a new unique UID for objects that have none.
    pub new_uid: &'a mut dyn FnMut() -> String,
}

fn property(name: &str, value: impl Into<String>) -> Property {
    Property {
        name: name.to_owned(),
        parameters: Vec::new(),
        value: value.into(),
    }
}

fn uid_of(component: &Component) -> Option<String> {
    component.property("UID").map(|p| p.value.clone())
}

fn serialize(component: &Component) -> Result<Vec<u8>, IcalError> {
    let text = to_string(component)?;
    if text.len() > MAX_OBJECT_SIZE {
        return Err(IcalError::TooLarge);
    }
    Ok(text.into_bytes())
}

/// Converts an import file into individual objects.
///
/// Fails as a whole if the file cannot be parsed, is too large, uses a
/// format that does not fit the collection or contains more than
/// [`MAX_IMPORT_OBJECTS`] objects. Problems with single objects are
/// reported per object.
pub fn import(
    data: &[u8],
    format: Format,
    target: Target,
    ctx: &mut ImportContext<'_>,
) -> Result<Vec<ImportedObject>, IcalError> {
    if data.len() > MAX_IMPORT_SIZE {
        return Err(IcalError::TooLarge);
    }
    if !format.supports(target) {
        return Err(IcalError::Invalid("format does not fit the collection"));
    }
    let mut objects = Vec::new();
    let roots = match format {
        Format::ICalendar | Format::VCard => parse_all(data, &IMPORT_LIMITS)?,
        Format::JCal => jcal::read(data, Dialect::ICal, &IMPORT_LIMITS)?,
        Format::JCard => jcal::read(data, Dialect::VCard, &IMPORT_LIMITS)?,
        Format::XCal => xcal::read(data, &IMPORT_LIMITS)?,
        Format::Csv => {
            let text = std::str::from_utf8(data).map_err(|_| IcalError::Malformed("not UTF-8"))?;
            let rows = csv::parse(text, MAX_IMPORT_OBJECTS + 1)?;
            match target {
                Target::Calendar => csv_events(&rows, ctx, &mut objects)?,
                Target::Addressbook => csv_contacts(&rows, ctx, &mut objects)?,
            }
        }
    };
    match target {
        Target::Calendar => split_calendars(roots, ctx, &mut objects),
        Target::Addressbook => split_cards(roots, ctx, &mut objects),
    }
    if objects.len() > MAX_IMPORT_OBJECTS {
        return Err(IcalError::Limit("too many objects in import"));
    }
    Ok(objects)
}

/// TZIDs referenced by parameters anywhere in the component.
fn referenced_tzids(component: &Component, out: &mut HashSet<String>) {
    let mut stack = vec![component];
    while let Some(component) = stack.pop() {
        for property in &component.properties {
            if let Some(tzid) = property.parameter("TZID") {
                out.extend(tzid.values.iter().cloned());
            }
        }
        stack.extend(component.components.iter());
    }
}

fn split_calendars(
    roots: Vec<Component>,
    ctx: &mut ImportContext<'_>,
    objects: &mut Vec<ImportedObject>,
) {
    let mut timezones: Vec<(String, Component)> = Vec::new();
    let mut groups: Vec<(String, Vec<Component>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut header: Vec<Property> = Vec::new();

    for root in roots {
        if root.name != "VCALENDAR" {
            objects.push(ImportedObject {
                uid: None,
                data: Err(IcalError::Invalid("top-level component must be VCALENDAR")),
            });
            continue;
        }
        if header.is_empty() {
            header.push(property("VERSION", "2.0"));
            header.push(
                root.property("PRODID")
                    .cloned()
                    .unwrap_or_else(|| property("PRODID", PRODID)),
            );
            if let Some(calscale) = root.property("CALSCALE") {
                header.push(calscale.clone());
            }
        }
        for mut component in root.components {
            match component.name.as_str() {
                "VTIMEZONE" => {
                    let tzid = component.property("TZID").map(|p| p.value.clone());
                    if let Some(tzid) = tzid
                        && !timezones.iter().any(|(t, _)| *t == tzid)
                    {
                        timezones.push((tzid, component));
                    }
                }
                "VEVENT" | "VTODO" | "VJOURNAL" => {
                    let uid = match uid_of(&component) {
                        Some(uid) => uid,
                        None => {
                            let uid = (ctx.new_uid)();
                            component.properties.insert(0, property("UID", uid.clone()));
                            uid
                        }
                    };
                    match index.get(&uid) {
                        Some(&i) => groups[i].1.push(component),
                        None => {
                            index.insert(uid.clone(), groups.len());
                            groups.push((uid, vec![component]));
                        }
                    }
                }
                _ => objects.push(ImportedObject {
                    uid: uid_of(&component),
                    data: Err(IcalError::Invalid("unsupported component type")),
                }),
            }
        }
    }

    for (uid, components) in groups {
        let mut tzids = HashSet::new();
        for component in &components {
            referenced_tzids(component, &mut tzids);
        }
        let mut children: Vec<Component> = timezones
            .iter()
            .filter(|(tzid, _)| tzids.contains(tzid))
            .map(|(_, tz)| tz.clone())
            .collect();
        children.extend(components);
        let calendar = Component {
            name: "VCALENDAR".to_owned(),
            properties: header.clone(),
            components: children,
        };
        objects.push(ImportedObject {
            uid: Some(uid),
            data: serialize(&calendar),
        });
    }
}

fn split_cards(
    roots: Vec<Component>,
    ctx: &mut ImportContext<'_>,
    objects: &mut Vec<ImportedObject>,
) {
    for mut card in roots {
        if card.name != "VCARD" {
            objects.push(ImportedObject {
                uid: None,
                data: Err(IcalError::Invalid("top-level component must be VCARD")),
            });
            continue;
        }
        let uid = match uid_of(&card) {
            Some(uid) => uid,
            None => {
                let uid = (ctx.new_uid)();
                // Directly after VERSION, which conventionally comes first.
                let at = usize::from(card.properties.first().is_some_and(|p| p.name == "VERSION"));
                card.properties.insert(at, property("UID", uid.clone()));
                uid
            }
        };
        objects.push(ImportedObject {
            uid: Some(uid),
            data: serialize(&card),
        });
    }
}

/// Column lookup by header name (case-insensitive, `_`/`-`/space ignored).
struct Columns(Vec<String>);

impl Columns {
    fn new(header: &[String]) -> Self {
        Self(header.iter().map(|h| normalise(h)).collect())
    }

    fn find(&self, names: &[&str]) -> Option<usize> {
        self.0.iter().position(|h| names.contains(&h.as_str()))
    }
}

fn normalise(name: &str) -> String {
    name.trim()
        .chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

fn cell(row: &[String], column: Option<usize>) -> &str {
    column.and_then(|i| row.get(i)).map_or("", |c| c.as_str())
}

fn is_true(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "y" | "1" | "x" | "ja" | "wahr"
    )
}

/// Parses a CSV date or date-time into iCalendar basic form.
fn csv_date(value: &str) -> Result<(String, bool), IcalError> {
    let invalid = IcalError::Invalid("invalid date in CSV");
    let compact: String = value
        .trim()
        .replace(' ', "T")
        .chars()
        .filter(|c| *c != '-' && *c != ':')
        .collect();
    let (date, time) = match compact.split_once('T') {
        Some((date, time)) => (date, Some(time)),
        None => (compact.as_str(), None),
    };
    let basic = match time {
        None => date.to_owned(),
        Some(time) => {
            let (body, zone) = match time.strip_suffix('Z') {
                Some(body) => (body, "Z"),
                None => (time, ""),
            };
            let body = match body.len() {
                4 => format!("{body}00"),
                6 => body.to_owned(),
                _ => return Err(invalid),
            };
            format!("{date}T{body}{zone}")
        }
    };
    let parsed = parse_date_or_datetime(&basic).map_err(|_| invalid)?;
    Ok((basic, parsed.is_date))
}

fn date_property(name: &str, value: &str, is_date: bool) -> Property {
    Property {
        name: name.to_owned(),
        parameters: if is_date {
            vec![Parameter {
                name: "VALUE".to_owned(),
                values: vec!["DATE".to_owned()],
            }]
        } else {
            Vec::new()
        },
        value: value.to_owned(),
    }
}

fn csv_event(
    row: &[String],
    columns: &Columns,
    ctx: &mut ImportContext<'_>,
) -> Result<Component, IcalError> {
    let col = |names: &[&str]| columns.find(names);
    let uid = cell(row, col(&["uid", "id"])).trim().to_owned();
    let uid = if uid.is_empty() { (ctx.new_uid)() } else { uid };
    let mut event = Component {
        name: "VEVENT".to_owned(),
        properties: vec![
            property("UID", escape_text(&uid)),
            property("DTSTAMP", ctx.dtstamp),
        ],
        components: Vec::new(),
    };
    let all_day = is_true(cell(row, col(&["allday", "wholeday"])));
    let start = cell(row, col(&["start", "dtstart", "startdate", "begin"]));
    if start.trim().is_empty() {
        return Err(IcalError::Invalid("CSV row without start"));
    }
    let (mut start, mut start_is_date) = csv_date(start)?;
    if all_day && !start_is_date {
        start.truncate(8);
        start_is_date = true;
    }
    event
        .properties
        .push(date_property("DTSTART", &start, start_is_date));
    let end = cell(row, col(&["end", "dtend", "enddate", "finish"]));
    if !end.trim().is_empty() {
        let (mut end, end_is_date) = csv_date(end)?;
        if start_is_date && !end_is_date {
            end.truncate(8);
        } else if !start_is_date && end_is_date {
            return Err(IcalError::Invalid(
                "CSV end is a date but start is a date-time",
            ));
        }
        // An all-day event ending on its start day lasts one day.
        if !(start_is_date && end <= start) {
            event
                .properties
                .push(date_property("DTEND", &end, start_is_date));
        }
    }
    for (names, name) in [
        (&["summary", "title", "subject", "name"][..], "SUMMARY"),
        (&["location", "place"][..], "LOCATION"),
        (&["description", "notes", "note"][..], "DESCRIPTION"),
    ] {
        let value = cell(row, col(names));
        let value = if name == "DESCRIPTION" {
            value
        } else {
            value.trim()
        };
        if !value.is_empty() {
            event.properties.push(property(name, escape_text(value)));
        }
    }
    Ok(Component {
        name: "VCALENDAR".to_owned(),
        properties: vec![property("VERSION", "2.0"), property("PRODID", PRODID)],
        components: vec![event],
    })
}

fn csv_events(
    rows: &[Vec<String>],
    ctx: &mut ImportContext<'_>,
    objects: &mut Vec<ImportedObject>,
) -> Result<Vec<Component>, IcalError> {
    let Some((header, rows)) = rows.split_first() else {
        return Ok(Vec::new());
    };
    let columns = Columns::new(header);
    if columns
        .find(&["start", "dtstart", "startdate", "begin"])
        .is_none()
    {
        return Err(IcalError::Invalid("CSV header needs a start column"));
    }
    let mut calendars = Vec::new();
    for row in rows {
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        match csv_event(row, &columns, ctx) {
            Ok(calendar) => calendars.push(calendar),
            Err(err) => objects.push(ImportedObject {
                uid: Some(cell(row, columns.find(&["uid", "id"])).to_owned())
                    .filter(|u| !u.is_empty()),
                data: Err(err),
            }),
        }
    }
    Ok(calendars)
}

fn list(value: &str) -> impl Iterator<Item = &str> {
    value
        .split([';', '\n'])
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn csv_contacts(
    rows: &[Vec<String>],
    ctx: &mut ImportContext<'_>,
    objects: &mut Vec<ImportedObject>,
) -> Result<Vec<Component>, IcalError> {
    let Some((header, rows)) = rows.split_first() else {
        return Ok(Vec::new());
    };
    let columns = Columns::new(header);
    let name_col = columns.find(&["name", "fn", "fullname", "displayname"]);
    if name_col.is_none() {
        return Err(IcalError::Invalid("CSV header needs a name column"));
    }
    let col = |names: &[&str]| columns.find(names);
    let mut cards = Vec::new();
    for row in rows {
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let name = cell(row, name_col).trim();
        let uid = cell(row, col(&["uid", "id"])).trim().to_owned();
        if name.is_empty() {
            objects.push(ImportedObject {
                uid: Some(uid).filter(|u| !u.is_empty()),
                data: Err(IcalError::Invalid("CSV row without name")),
            });
            continue;
        }
        let uid = if uid.is_empty() { (ctx.new_uid)() } else { uid };
        let mut card = Component {
            name: "VCARD".to_owned(),
            properties: vec![
                property("VERSION", "4.0"),
                property("UID", escape_text(&uid)),
                property("PRODID", PRODID),
                property("FN", escape_text(name)),
            ],
            components: Vec::new(),
        };
        for email in list(cell(row, col(&["emails", "email", "mail"]))) {
            card.properties.push(property("EMAIL", escape_text(email)));
        }
        for phone in list(cell(row, col(&["phones", "phone", "tel", "telephone"]))) {
            card.properties.push(property("TEL", escape_text(phone)));
        }
        let org: Vec<String> = cell(
            row,
            col(&["org", "organization", "organisation", "company"]),
        )
        .split(';')
        .map(|c| escape_text(c.trim()))
        .collect();
        if org.iter().any(|c| !c.is_empty()) {
            card.properties.push(property("ORG", org.join(";")));
        }
        cards.push(card);
    }
    Ok(cards)
}

/// Days since 1970-01-01 to (year, month, day) (H. Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Formats a parsed DATE/DATE-TIME for CSV (`2026-10-21`,
/// `2026-10-21T10:00:00[Z]`).
fn csv_format(value: &DateTime) -> String {
    let (year, month, day) = civil_from_days(value.seconds.div_euclid(86_400));
    if value.is_date {
        return format!("{year:04}-{month:02}-{day:02}");
    }
    let secs = value.seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        if value.is_utc { "Z" } else { "" }
    )
}

fn text_of(component: &Component, name: &str) -> String {
    component
        .property(name)
        .map(|p| unescape_text(&p.value))
        .unwrap_or_default()
}

const EVENT_COLUMNS: [&str; 7] = [
    "summary",
    "start",
    "end",
    "all_day",
    "location",
    "description",
    "uid",
];
const CONTACT_COLUMNS: [&str; 5] = ["name", "emails", "phones", "org", "uid"];

fn event_row(component: &Component) -> [String; 7] {
    let date = |name: &str| {
        component
            .property(name)
            .and_then(|p| parse_date_or_datetime(&p.value).ok())
    };
    let start = date("DTSTART");
    let end = date("DTEND").or_else(|| date("DUE")).or_else(|| {
        let start = start?;
        let duration = parse_duration(&component.property("DURATION")?.value).ok()?;
        Some(DateTime {
            seconds: start.seconds.checked_add(duration)?,
            ..start
        })
    });
    [
        text_of(component, "SUMMARY"),
        start.as_ref().map(csv_format).unwrap_or_default(),
        end.as_ref().map(csv_format).unwrap_or_default(),
        start
            .map_or("", |s| if s.is_date { "true" } else { "false" })
            .to_owned(),
        text_of(component, "LOCATION"),
        text_of(component, "DESCRIPTION"),
        text_of(component, "UID"),
    ]
}

fn contact_row(card: &Component) -> [String; 5] {
    let all = |name: &str| {
        card.properties_named(name)
            .map(|p| unescape_text(&p.value))
            .map(|v| v.strip_prefix("tel:").map(str::to_owned).unwrap_or(v))
            .collect::<Vec<_>>()
            .join("; ")
    };
    let org = card
        .property("ORG")
        .map(|p| {
            split_unescaped(&p.value, ';')
                .into_iter()
                .map(unescape_text)
                .filter(|c| !c.is_empty())
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();
    [
        text_of(card, "FN"),
        all("EMAIL"),
        all("TEL"),
        org,
        text_of(card, "UID"),
    ]
}

/// Writes a collection in one format, object by object.
pub struct Exporter {
    format: Format,
    target: Target,
    first: bool,
    timezones: HashSet<String>,
}

impl Exporter {
    pub fn new(format: Format, target: Target) -> Result<Self, IcalError> {
        if !format.supports(target) {
            return Err(IcalError::Invalid("format does not fit the collection"));
        }
        Ok(Self {
            format,
            target,
            first: true,
            timezones: HashSet::new(),
        })
    }

    fn calendar_header(name: Option<&str>) -> Component {
        let mut properties = vec![
            property("VERSION", "2.0"),
            property("PRODID", PRODID),
            property("CALSCALE", "GREGORIAN"),
        ];
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            properties.push(property("X-WR-CALNAME", escape_text(name)));
        }
        Component {
            name: "VCALENDAR".to_owned(),
            properties,
            components: Vec::new(),
        }
    }

    /// The start of the file. `name` is the collection's display name.
    pub fn begin(&mut self, name: Option<&str>) -> Result<String, IcalError> {
        let mut out = String::new();
        match (self.format, self.target) {
            (Format::ICalendar, _) => {
                out.push_str("BEGIN:VCALENDAR\r\n");
                for property in &Self::calendar_header(name).properties {
                    write_property(property, &mut out)?;
                }
            }
            (Format::JCal, _) => jcal::write_open(&Self::calendar_header(name), &mut out),
            (Format::XCal, _) => {
                out.push_str(&xcal::document_start());
                xcal::write_open(&Self::calendar_header(name), &mut out);
            }
            (Format::JCard, _) => out.push('['),
            (Format::VCard, _) => {}
            (Format::Csv, Target::Calendar) => {
                // A BOM lets spreadsheet applications detect UTF-8.
                out.push('\u{feff}');
                csv::push_row(&EVENT_COLUMNS, &mut out);
            }
            (Format::Csv, Target::Addressbook) => {
                out.push('\u{feff}');
                csv::push_row(&CONTACT_COLUMNS, &mut out);
            }
        }
        Ok(out)
    }

    /// One stored object (validated iCalendar or vCard text).
    pub fn object(&mut self, data: &[u8]) -> Result<String, IcalError> {
        let root = parse(data)?;
        let mut out = String::new();
        match self.target {
            Target::Calendar => {
                if root.name != "VCALENDAR" {
                    return Err(IcalError::Invalid("not a calendar object"));
                }
                if self.format == Format::Csv {
                    // CSV holds events only; to-dos and journals are left out.
                    let master = root
                        .components
                        .iter()
                        .filter(|c| c.name == "VEVENT")
                        .find(|c| c.property("RECURRENCE-ID").is_none())
                        .or_else(|| root.components.iter().find(|c| c.name == "VEVENT"));
                    if let Some(master) = master {
                        csv::push_row(&event_row(master), &mut out);
                    }
                    return Ok(out);
                }
                for component in &root.components {
                    if component.name == "VTIMEZONE" {
                        let tzid = component.property("TZID").map(|p| p.value.clone());
                        if !tzid.is_some_and(|tzid| self.timezones.insert(tzid)) {
                            continue;
                        }
                    }
                    match self.format {
                        Format::ICalendar => {
                            crate::write::write_component(component, &mut out)?;
                        }
                        Format::JCal => {
                            if !std::mem::take(&mut self.first) {
                                out.push(',');
                            }
                            jcal::write_component(component, Dialect::ICal, &mut out);
                        }
                        Format::XCal => xcal::write_component(component, &mut out),
                        _ => {}
                    }
                }
            }
            Target::Addressbook => {
                if root.name != "VCARD" {
                    return Err(IcalError::Invalid("not a vCard"));
                }
                match self.format {
                    Format::VCard => crate::write::write_component(&root, &mut out)?,
                    Format::JCard => {
                        if !std::mem::take(&mut self.first) {
                            out.push(',');
                        }
                        jcal::write_component(&root, Dialect::VCard, &mut out);
                    }
                    Format::Csv => csv::push_row(&contact_row(&root), &mut out),
                    _ => {}
                }
            }
        }
        Ok(out)
    }

    /// The end of the file.
    pub fn end(&mut self) -> String {
        match self.format {
            Format::ICalendar => "END:VCALENDAR\r\n".to_owned(),
            Format::JCal => "]]\n".to_owned(),
            Format::XCal => format!("</components></vcalendar>{}", xcal::DOCUMENT_END),
            Format::JCard => "]\n".to_owned(),
            Format::VCard | Format::Csv => String::new(),
        }
    }
}
