//! xCal (RFC 6321): the XML form of iCalendar.
//!
//! Input is parsed with the single hardened XML parser ([`crate::xml`]):
//! no DTDs, no entities, bounded depth and element count.

use crate::{
    Component, IcalError,
    parser::{Limits, MAX_DEPTH, is_name},
    values::{Dialect, TypedProperty, TypedValue, from_typed, to_typed},
    xml::{self, Node, XmlError, XmlLimits, escape, is_xml_name},
};

/// Namespace of xCal elements.
pub const NS_XCAL: &str = "urn:ietf:params:xml:ns:icalendar-2.0";

/// Opening of an xCal document.
pub(crate) fn document_start() -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<icalendar xmlns=\"{NS_XCAL}\">")
}

pub(crate) const DOCUMENT_END: &str = "</icalendar>\n";

fn element(name: &str, text: &str, out: &mut String) {
    out.push('<');
    out.push_str(name);
    out.push('>');
    out.push_str(&escape(text));
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}

/// Value type element of a parameter (RFC 6321 §3.5).
fn parameter_type(name: &str) -> &'static str {
    match name {
        "delegated-from" | "delegated-to" | "member" | "sent-by" => "cal-address",
        "altrep" | "dir" => "uri",
        _ => "text",
    }
}

fn write_value(property: &TypedProperty, value: &TypedValue, out: &mut String) {
    // Only valid XML names can become elements; anything else is unknown.
    let vtype = if is_xml_name(&property.vtype) {
        property.vtype.as_str()
    } else {
        "unknown"
    };
    match value {
        TypedValue::Text(text) | TypedValue::Number(text) => element(vtype, text, out),
        TypedValue::Bool(b) => element(vtype, if *b { "true" } else { "false" }, out),
        TypedValue::Structured(components) => {
            if property.name == "request-status" {
                for (name, values) in ["code", "description", "data"].iter().zip(components) {
                    element(name, &values.join(","), out);
                }
            } else {
                for values in components {
                    element(vtype, &values.join(","), out);
                }
            }
        }
        TypedValue::Recur(parts) => {
            out.push_str("<recur>");
            for (key, values) in parts {
                for value in values {
                    element(key, value, out);
                }
            }
            out.push_str("</recur>");
        }
        TypedValue::Period(start, end) => {
            out.push_str("<period>");
            element("start", start, out);
            let end_name = if end.starts_with(['P', '+', '-']) {
                "duration"
            } else {
                "end"
            };
            element(end_name, end, out);
            out.push_str("</period>");
        }
        TypedValue::Geo(lat, lon) => {
            element("latitude", lat, out);
            element("longitude", lon, out);
        }
    }
}

fn write_property(property: &TypedProperty, out: &mut String) {
    out.push('<');
    out.push_str(&property.name);
    out.push('>');
    let params: Vec<_> = property
        .params
        .iter()
        .filter(|(name, _)| is_xml_name(name))
        .collect();
    if !params.is_empty() {
        out.push_str("<parameters>");
        for (name, values) in params {
            out.push('<');
            out.push_str(name);
            out.push('>');
            for value in values {
                element(parameter_type(name), value, out);
            }
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
        out.push_str("</parameters>");
    }
    for value in &property.values {
        write_value(property, value, out);
    }
    out.push_str("</");
    out.push_str(&property.name);
    out.push('>');
}

fn push_head(component: &Component, out: &mut String) -> String {
    let name = component.name.to_ascii_lowercase();
    out.push('<');
    out.push_str(&name);
    out.push_str("><properties>");
    for property in &component.properties {
        let typed = to_typed(property, Dialect::ICal);
        if is_xml_name(&typed.name) {
            write_property(&typed, out);
        }
    }
    out.push_str("</properties>");
    name
}

/// Appends a component's start tag and properties and opens its
/// `<components>`; the caller closes both.
pub(crate) fn write_open(component: &Component, out: &mut String) {
    push_head(component, out);
    out.push_str("<components>");
}

/// Appends the xCal form of a component. Properties whose names are not
/// valid XML names (e.g. starting with a digit) cannot be represented and
/// are left out.
pub(crate) fn write_component(component: &Component, out: &mut String) {
    let name = push_head(component, out);
    let children: Vec<&Component> = component
        .components
        .iter()
        .filter(|c| is_xml_name(&c.name))
        .collect();
    if !children.is_empty() {
        out.push_str("<components>");
        for child in children {
            write_component(child, out);
        }
        out.push_str("</components>");
    }
    out.push_str("</");
    out.push_str(&name);
    out.push('>');
}

const MALFORMED: IcalError = IcalError::Malformed("invalid xCal structure");

fn xcal_children(node: &Node) -> impl Iterator<Item = &Node> {
    node.children.iter().filter(|c| c.namespace == NS_XCAL)
}

struct Reader<'a> {
    limits: &'a Limits,
    components: usize,
    properties: usize,
}

impl Reader<'_> {
    fn property(&mut self, node: &Node) -> Result<crate::Property, IcalError> {
        self.properties += 1;
        if self.properties > self.limits.max_properties {
            return Err(IcalError::Limit("too many properties"));
        }
        let name = node.name.clone();
        let mut params = Vec::new();
        let mut vtype: Option<String> = None;
        let mut values = Vec::new();
        let mut rstatus: Vec<Vec<String>> = Vec::new();
        let mut geo: Vec<String> = Vec::new();
        for child in xcal_children(node) {
            match child.name.as_str() {
                "parameters" => {
                    for param in xcal_children(child) {
                        let param_values: Vec<String> =
                            xcal_children(param).map(|v| v.text.clone()).collect();
                        params.push((param.name.clone(), param_values));
                    }
                }
                "code" | "description" | "data" if name == "request-status" => {
                    rstatus.push(vec![child.text.clone()]);
                    vtype.get_or_insert_with(|| "text".to_owned());
                }
                "latitude" | "longitude" if name == "geo" => {
                    geo.push(child.text.trim().to_owned());
                    vtype.get_or_insert_with(|| "float".to_owned());
                }
                "recur" => {
                    let mut parts: Vec<(String, Vec<String>)> = Vec::new();
                    for part in xcal_children(child) {
                        match parts.iter_mut().find(|(k, _)| *k == part.name) {
                            Some((_, values)) => values.push(part.text.trim().to_owned()),
                            None => {
                                parts.push((part.name.clone(), vec![part.text.trim().to_owned()]))
                            }
                        }
                    }
                    vtype.get_or_insert_with(|| "recur".to_owned());
                    values.push(TypedValue::Recur(parts));
                }
                "period" => {
                    let start = child.child(NS_XCAL, "start").ok_or(MALFORMED)?;
                    let end = child
                        .child(NS_XCAL, "end")
                        .or_else(|| child.child(NS_XCAL, "duration"))
                        .ok_or(MALFORMED)?;
                    vtype.get_or_insert_with(|| "period".to_owned());
                    values.push(TypedValue::Period(
                        start.text.trim().to_owned(),
                        end.text.trim().to_owned(),
                    ));
                }
                other => {
                    let vtype = vtype.get_or_insert_with(|| other.to_owned());
                    values.push(match vtype.as_str() {
                        "integer" | "float" => TypedValue::Number(child.text.trim().to_owned()),
                        "boolean" => TypedValue::Bool(child.text.trim() == "true"),
                        "date" | "date-time" | "time" | "utc-offset" | "duration" => {
                            TypedValue::Text(child.text.trim().to_owned())
                        }
                        _ => TypedValue::Text(child.text.clone()),
                    });
                }
            }
        }
        if !rstatus.is_empty() {
            values.push(TypedValue::Structured(rstatus));
        }
        if let [lat, lon] = geo.as_slice() {
            values.push(TypedValue::Geo(lat.clone(), lon.clone()));
        } else if !geo.is_empty() {
            return Err(MALFORMED);
        }
        from_typed(
            &TypedProperty {
                name,
                params,
                vtype: vtype.ok_or(MALFORMED)?,
                values,
            },
            Dialect::ICal,
        )
    }

    fn component(&mut self, node: &Node, depth: usize) -> Result<Component, IcalError> {
        if depth >= MAX_DEPTH {
            return Err(IcalError::Limit("components nested too deeply"));
        }
        self.components += 1;
        if self.components > self.limits.max_components {
            return Err(IcalError::Limit("too many components"));
        }
        if !is_name(&node.name) {
            return Err(IcalError::Malformed("invalid component name"));
        }
        let mut component = Component {
            name: node.name.to_ascii_uppercase(),
            ..Component::default()
        };
        for child in xcal_children(node) {
            match child.name.as_str() {
                "properties" => {
                    for property in xcal_children(child) {
                        component.properties.push(self.property(property)?);
                    }
                }
                "components" => {
                    for sub in xcal_children(child) {
                        component.components.push(self.component(sub, depth + 1)?);
                    }
                }
                _ => return Err(MALFORMED),
            }
        }
        Ok(component)
    }
}

/// Parses an xCal document into its `VCALENDAR` components.
pub(crate) fn read(data: &[u8], limits: &Limits) -> Result<Vec<Component>, IcalError> {
    let xml_limits = XmlLimits {
        max_len: limits.max_size,
        // icalendar/vcalendar/components/…/property/parameters/param/value
        max_depth: 2 * MAX_DEPTH + 8,
        // Each property needs at least two elements.
        max_nodes: limits.max_properties.saturating_mul(2),
    };
    let root = xml::parse(data, &xml_limits).map_err(|err| match err {
        XmlError::TooLarge => IcalError::TooLarge,
        XmlError::Invalid(message) => IcalError::Malformed(message),
    })?;
    if !root.is(NS_XCAL, "icalendar") {
        return Err(IcalError::Malformed("root element must be xCal icalendar"));
    }
    let mut reader = Reader {
        limits,
        components: 0,
        properties: 0,
    };
    xcal_children(&root)
        .map(|c| reader.component(c, 0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        max_size: 1 << 20,
        max_components: 1000,
        max_properties: 10_000,
    };

    #[test]
    fn rejects_dtd_entities_and_foreign_roots() {
        for bad in [
            format!(
                "<!DOCTYPE i [<!ENTITY a \"aaaa\">]><icalendar xmlns=\"{NS_XCAL}\">&a;</icalendar>"
            ),
            format!("<!DOCTYPE i SYSTEM \"file:///etc/passwd\"><icalendar xmlns=\"{NS_XCAL}\"/>"),
            "<icalendar/>".to_owned(),
            format!("<icalendar xmlns=\"{NS_XCAL}\"><vcalendar><bogus/></vcalendar></icalendar>"),
            format!(
                "<icalendar xmlns=\"{NS_XCAL}\"><vcalendar><properties><summary/></properties></vcalendar></icalendar>"
            ),
        ] {
            assert!(read(bad.as_bytes(), &LIMITS).is_err(), "{bad}");
        }
        let deep = format!(
            "<icalendar xmlns=\"{NS_XCAL}\">{}{}</icalendar>",
            "<vcalendar><components>".repeat(20),
            "</components></vcalendar>".repeat(20)
        );
        assert!(read(deep.as_bytes(), &LIMITS).is_err());
    }
}
