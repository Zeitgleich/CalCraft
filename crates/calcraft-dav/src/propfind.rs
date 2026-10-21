//! PROPFIND request bodies (RFC 4918 §9.1).

use quick_xml::{NsReader, events::Event, name::ResolveResult};

use crate::{DavError, xml::NS_DAV};

/// Largest accepted PROPFIND body. Real clients send a few kilobytes.
pub const MAX_PROPFIND_BODY: usize = 64 * 1024;

const MAX_DEPTH: usize = 16;
const MAX_PROPS: usize = 256;
const MAX_NAME_LEN: usize = 256;

/// A property name: namespace URI plus local name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PropName {
    pub namespace: String,
    pub name: String,
}

impl PropName {
    pub fn new(namespace: &str, name: &str) -> Self {
        Self {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
        }
    }

    pub fn is(&self, namespace: &str, name: &str) -> bool {
        self.namespace == namespace && self.name == name
    }
}

/// What a PROPFIND asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Propfind {
    AllProp,
    PropName,
    Prop(Vec<PropName>),
}

impl Propfind {
    /// Parses a PROPFIND body. An empty body means `allprop`.
    ///
    /// Document type declarations are rejected, which rules out entity
    /// expansion attacks; nesting depth and property count are bounded.
    pub fn parse(body: &[u8]) -> Result<Self, DavError> {
        if body.len() > MAX_PROPFIND_BODY {
            return Err(DavError::TooLarge);
        }
        if body.trim_ascii().is_empty() {
            return Ok(Self::AllProp);
        }
        let text =
            std::str::from_utf8(body).map_err(|_| DavError::BadRequest("body is not UTF-8"))?;

        let mut reader = NsReader::from_str(text);
        reader.config_mut().expand_empty_elements = true;
        reader.config_mut().trim_text(true);

        // Element path below the root, as (is DAV namespace, local name).
        let mut stack: Vec<(bool, String)> = Vec::new();
        let mut kind: Option<Propfind> = None;
        let mut props: Vec<PropName> = Vec::new();

        loop {
            let (ns, event) = reader
                .read_resolved_event()
                .map_err(|_| DavError::BadRequest("malformed XML"))?;
            match event {
                Event::Start(start) => {
                    if stack.len() >= MAX_DEPTH {
                        return Err(DavError::BadRequest("XML nested too deeply"));
                    }
                    let local_name = start.local_name();
                    let local: &str = local_name.into_inner();
                    if local.len() > MAX_NAME_LEN {
                        return Err(DavError::BadRequest("element name too long"));
                    }
                    let namespace = match ns {
                        ResolveResult::Bound(ns) => ns.0,
                        ResolveResult::Unbound => "",
                        ResolveResult::Unknown(_) => {
                            return Err(DavError::BadRequest("undeclared namespace prefix"));
                        }
                    };
                    if namespace.len() > MAX_NAME_LEN {
                        return Err(DavError::BadRequest("namespace too long"));
                    }
                    let is_dav = namespace == NS_DAV;
                    match stack.as_slice() {
                        [] if !(is_dav && local == "propfind") => {
                            return Err(DavError::BadRequest("root element must be DAV:propfind"));
                        }
                        [(true, root)] if root == "propfind" && is_dav => {
                            let next = match local {
                                "allprop" => Some(Propfind::AllProp),
                                "propname" => Some(Propfind::PropName),
                                "prop" => Some(Propfind::Prop(Vec::new())),
                                // `include` (allprop extension) is ignored.
                                _ => None,
                            };
                            if let Some(next) = next {
                                if kind.is_some() {
                                    return Err(DavError::BadRequest(
                                        "more than one of allprop, propname and prop",
                                    ));
                                }
                                kind = Some(next);
                            }
                        }
                        [_, (true, parent)] if parent == "prop" => {
                            if props.len() >= MAX_PROPS {
                                return Err(DavError::BadRequest("too many properties"));
                            }
                            props.push(PropName::new(namespace, local));
                        }
                        _ => {}
                    }
                    stack.push((is_dav, local.to_owned()));
                }
                Event::End(_) => {
                    stack.pop();
                }
                Event::DocType(_) => {
                    return Err(DavError::BadRequest(
                        "document type declarations are not allowed",
                    ));
                }
                Event::GeneralRef(_) => {
                    return Err(DavError::BadRequest("entity references are not allowed"));
                }
                Event::Eof if stack.is_empty() => break,
                Event::Eof => return Err(DavError::BadRequest("unexpected end of XML")),
                Event::Text(_)
                | Event::CData(_)
                | Event::Comment(_)
                | Event::Decl(_)
                | Event::PI(_)
                | Event::Empty(_) => {}
            }
        }

        match kind {
            Some(Propfind::Prop(_)) => Ok(Propfind::Prop(props)),
            Some(other) => Ok(other),
            None => Err(DavError::BadRequest(
                "propfind without allprop, propname or prop",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::NS_CALDAV;

    #[test]
    fn empty_body_is_allprop() {
        assert_eq!(Propfind::parse(b""), Ok(Propfind::AllProp));
        assert_eq!(Propfind::parse(b"  \n"), Ok(Propfind::AllProp));
    }

    #[test]
    fn parses_prop_with_namespaces() {
        let body = br#"<?xml version="1.0" encoding="utf-8"?>
            <d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
              <d:prop>
                <d:current-user-principal/>
                <c:calendar-home-set/>
                <x:unknown xmlns:x="http://example.com/ns"/>
              </d:prop>
            </d:propfind>"#;
        let Propfind::Prop(props) = Propfind::parse(body).unwrap() else {
            panic!("expected prop");
        };
        assert_eq!(
            props,
            vec![
                PropName::new(NS_DAV, "current-user-principal"),
                PropName::new(NS_CALDAV, "calendar-home-set"),
                PropName::new("http://example.com/ns", "unknown"),
            ]
        );
    }

    #[test]
    fn default_namespace_works() {
        let body = br#"<propfind xmlns="DAV:"><prop><displayname/></prop></propfind>"#;
        assert_eq!(
            Propfind::parse(body),
            Ok(Propfind::Prop(vec![PropName::new(NS_DAV, "displayname")]))
        );
    }

    #[test]
    fn parses_allprop_and_propname() {
        assert_eq!(
            Propfind::parse(br#"<d:propfind xmlns:d="DAV:"><d:allprop/></d:propfind>"#),
            Ok(Propfind::AllProp)
        );
        assert_eq!(
            Propfind::parse(br#"<d:propfind xmlns:d="DAV:"><d:propname/></d:propfind>"#),
            Ok(Propfind::PropName)
        );
    }

    #[test]
    fn rejects_wrong_root() {
        assert!(Propfind::parse(br#"<propfind><prop/></propfind>"#).is_err());
        assert!(Propfind::parse(br#"<d:other xmlns:d="DAV:"/>"#).is_err());
    }

    #[test]
    fn rejects_doctype_and_entities() {
        let billion_laughs = br#"<?xml version="1.0"?>
            <!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;">]>
            <d:propfind xmlns:d="DAV:"><d:prop><d:displayname>&lol2;</d:displayname></d:prop></d:propfind>"#;
        assert!(Propfind::parse(billion_laughs).is_err());

        let xxe = br#"<?xml version="1.0"?>
            <!DOCTYPE d [<!ENTITY xxe SYSTEM "file:///etc/passwd">]>
            <d:propfind xmlns:d="DAV:"><d:prop>&xxe;</d:prop></d:propfind>"#;
        assert!(Propfind::parse(xxe).is_err());
    }

    #[test]
    fn rejects_deep_nesting() {
        let mut body = String::from(r#"<d:propfind xmlns:d="DAV:"><d:prop>"#);
        body.push_str(&"<a>".repeat(100));
        body.push_str(&"</a>".repeat(100));
        body.push_str("</d:prop></d:propfind>");
        assert!(Propfind::parse(body.as_bytes()).is_err());
    }

    #[test]
    fn rejects_too_many_props_and_large_bodies() {
        let mut body = String::from(r#"<d:propfind xmlns:d="DAV:"><d:prop>"#);
        body.push_str(&"<d:x/>".repeat(MAX_PROPS + 1));
        body.push_str("</d:prop></d:propfind>");
        assert!(Propfind::parse(body.as_bytes()).is_err());

        assert_eq!(
            Propfind::parse(&vec![b' '; MAX_PROPFIND_BODY + 1]),
            Err(DavError::TooLarge)
        );
    }

    #[test]
    fn rejects_malformed_xml() {
        for body in [
            &b"<d:propfind xmlns:d=\"DAV:\"><d:prop>"[..],
            b"<d:propfind xmlns:d=\"DAV:\"></d:prop>",
            b"<x:propfind/>",
            b"\xff\xfe",
        ] {
            assert!(Propfind::parse(body).is_err(), "{body:?}");
        }
    }
}
