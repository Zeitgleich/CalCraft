//! XML namespaces and serialisation helpers.

use quick_xml::escape::escape;

pub const NS_DAV: &str = "DAV:";
pub const NS_CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
pub const NS_CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
pub const NS_CALENDARSERVER: &str = "http://calendarserver.org/ns/";
pub const NS_APPLE_ICAL: &str = "http://apple.com/ns/ical/";

/// Namespace prefixes declared on every response root element.
pub(crate) const PREFIXES: [(&str, &str); 5] = [
    ("d", NS_DAV),
    ("c", NS_CALDAV),
    ("card", NS_CARDDAV),
    ("cs", NS_CALENDARSERVER),
    ("ic", NS_APPLE_ICAL),
];

pub(crate) fn prefix_for(namespace: &str) -> Option<&'static str> {
    PREFIXES
        .iter()
        .find(|(_, ns)| *ns == namespace)
        .map(|(prefix, _)| *prefix)
}

/// Escapes text content and attribute values.
pub fn escape_text(text: &str) -> std::borrow::Cow<'_, str> {
    escape(text)
}

/// The `xmlns` declarations for a response root element.
pub(crate) fn root_namespaces() -> String {
    PREFIXES
        .iter()
        .map(|(prefix, ns)| format!(" xmlns:{prefix}=\"{ns}\""))
        .collect()
}
