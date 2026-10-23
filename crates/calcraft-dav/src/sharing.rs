//! Request bodies of Apple's calendar sharing (CalendarServer
//! `caldav-sharing`): the owner's `CS:share` POST to a collection and the
//! sharee's `CS:invite-reply` POST to their home.
//!
//! ```xml
//! <CS:share xmlns:D="DAV:" xmlns:CS="http://calendarserver.org/ns/">
//!   <CS:set>
//!     <D:href>mailto:bob@example.org</D:href>
//!     <CS:summary>Team</CS:summary>
//!     <CS:read-write/>
//!   </CS:set>
//!   <CS:remove><D:href>/dav/principals/carol/</D:href></CS:remove>
//! </CS:share>
//!
//! <CS:invite-reply xmlns:D="DAV:" xmlns:CS="http://calendarserver.org/ns/">
//!   <D:href>mailto:alice@example.org</D:href>
//!   <CS:invite-accepted/>
//!   <CS:hosturl><D:href>/dav/calendars/alice/team/</D:href></CS:hosturl>
//!   <CS:in-reply-to>0123456789abcdef0123456789abcdef</CS:in-reply-to>
//! </CS:invite-reply>
//! ```
//!
//! Only the structure is checked here; whom an href names is up to the
//! server.

use crate::{
    DavError,
    tree::{self, Node},
    xml::{NS_CALENDARSERVER as CS, NS_DAV},
};

/// Largest accepted sharing body.
pub const MAX_SHARE_BODY: usize = 64 * 1024;
/// Most `set` plus `remove` entries in one `CS:share` request.
pub const MAX_SHARE_ENTRIES: usize = 100;
/// Longest href or text value taken from a sharing body.
const MAX_VALUE_LEN: usize = 2048;

/// One `CS:set`: share with `href` (a `mailto:` or principal URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareSet {
    pub href: String,
    /// `CS:read-write`; otherwise (`CS:read`) read only.
    pub read_write: bool,
}

/// A `CS:share` request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShareRequest {
    pub set: Vec<ShareSet>,
    /// Hrefs of sharees to remove.
    pub remove: Vec<String>,
}

/// A `CS:invite-reply` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteReply {
    /// `CS:invite-accepted` (else `CS:invite-declined`).
    pub accepted: bool,
    /// The shared collection's URL in the owner's home (`CS:hosturl`).
    pub host_url: Option<String>,
    /// The invitation's uid (`CS:in-reply-to`).
    pub in_reply_to: Option<String>,
}

/// A sharing POST body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharingPost {
    Share(ShareRequest),
    InviteReply(InviteReply),
}

fn value(text: &str) -> Result<String, DavError> {
    let text = text.trim();
    if text.is_empty() || text.len() > MAX_VALUE_LEN || text.chars().any(char::is_control) {
        return Err(DavError::BadRequest("invalid value in sharing request"));
    }
    Ok(text.to_owned())
}

fn href(node: &Node) -> Result<String, DavError> {
    let mut hrefs = node.children_named(NS_DAV, "href");
    let href = hrefs
        .next()
        .ok_or(DavError::BadRequest("DAV:href missing in sharing request"))?;
    if hrefs.next().is_some() {
        return Err(DavError::BadRequest("more than one DAV:href"));
    }
    value(&href.text)
}

impl ShareRequest {
    fn from_root(root: &Node) -> Result<Self, DavError> {
        let mut request = Self::default();
        for child in &root.children {
            if child.is(CS, "set") {
                let read = child.child(CS, "read").is_some();
                let read_write = child.child(CS, "read-write").is_some();
                if read == read_write {
                    return Err(DavError::BadRequest(
                        "CS:set needs exactly one of CS:read and CS:read-write",
                    ));
                }
                request.set.push(ShareSet {
                    href: href(child)?,
                    read_write,
                });
            } else if child.is(CS, "remove") {
                request.remove.push(href(child)?);
            } else {
                return Err(DavError::BadRequest("unknown element in CS:share"));
            }
            if request.set.len() + request.remove.len() > MAX_SHARE_ENTRIES {
                return Err(DavError::BadRequest("too many sharees in one request"));
            }
        }
        if request.set.is_empty() && request.remove.is_empty() {
            return Err(DavError::BadRequest("empty CS:share"));
        }
        Ok(request)
    }
}

impl InviteReply {
    fn from_root(root: &Node) -> Result<Self, DavError> {
        let accepted = root.child(CS, "invite-accepted").is_some();
        let declined = root.child(CS, "invite-declined").is_some();
        if accepted == declined {
            return Err(DavError::BadRequest(
                "CS:invite-reply needs exactly one of invite-accepted and invite-declined",
            ));
        }
        let host_url = root.child(CS, "hosturl").map(href).transpose()?;
        let in_reply_to = root
            .child(CS, "in-reply-to")
            .map(|node| value(&node.text))
            .transpose()?;
        if host_url.is_none() && in_reply_to.is_none() {
            return Err(DavError::BadRequest(
                "CS:invite-reply needs CS:hosturl or CS:in-reply-to",
            ));
        }
        Ok(Self {
            accepted,
            host_url,
            in_reply_to,
        })
    }
}

impl SharingPost {
    /// Parses a `CS:share` or `CS:invite-reply` body.
    pub fn parse(body: &[u8]) -> Result<Self, DavError> {
        if body.len() > MAX_SHARE_BODY {
            return Err(DavError::TooLarge);
        }
        let root = tree::parse(body, MAX_SHARE_BODY)?;
        if root.is(CS, "share") {
            ShareRequest::from_root(&root).map(Self::Share)
        } else if root.is(CS, "invite-reply") {
            InviteReply::from_root(&root).map(Self::InviteReply)
        } else {
            Err(DavError::BadRequest(
                "root element must be CS:share or CS:invite-reply",
            ))
        }
    }
}

#[cfg(test)]
mod tests;
