//! E-mail building blocks without I/O: SMTP reply parsing (RFC 5321
//! §4.2), message generation (RFC 5322 headers, RFC 2047 encoded-words,
//! RFC 2045 quoted-printable), dot-stuffing (RFC 5321 §4.5.2) and address
//! validation.
//!
//! Header injection is impossible by construction: every header value is
//! either validated (addresses, message id) or encoded (display names,
//! subjects), and the generated message is checked once more before it is
//! returned ([`check_message`]). The SMTP client in the server uses these
//! functions; they are also the target of a fuzzer.

use std::fmt::Write as _;

/// Longest reply line accepted (RFC 5321 allows 512 octets; generous).
pub const MAX_REPLY_LINE: usize = 1000;
/// Most lines of one multi-line reply (EHLO lists extensions).
pub const MAX_REPLY_LINES: usize = 64;
/// Longest line of a message (RFC 5322 §2.1.1, without CRLF).
pub const MAX_LINE: usize = 998;
/// Longest e-mail address (RFC 5321 path limit).
pub const MAX_ADDRESS_LEN: usize = 254;
/// Characters of a subject kept; longer ones are cut with "…".
pub const MAX_SUBJECT_CHARS: usize = 120;

/// A complete SMTP reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub code: u16,
    /// Text of each line (lossily decoded, never logged by CalCraft).
    pub lines: Vec<String>,
}

impl Reply {
    /// 2xx.
    pub fn is_positive(&self) -> bool {
        (200..300).contains(&self.code)
    }

    /// 4xx: try again later.
    pub fn is_transient(&self) -> bool {
        (400..500).contains(&self.code)
    }
}

/// A malformed or oversized reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReplyError {
    #[error("reply line too long")]
    LineTooLong,
    #[error("reply has too many lines")]
    TooManyLines,
    #[error("malformed reply line")]
    Malformed,
    #[error("reply code changed within a multi-line reply")]
    CodeMismatch,
}

/// Collects the lines of one (possibly multi-line) reply.
#[derive(Debug, Default)]
pub struct ReplyParser {
    code: Option<u16>,
    lines: Vec<String>,
}

impl ReplyParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one line (with or without its line end). Returns the reply
    /// once its last line (`250 text`) arrived.
    pub fn push(&mut self, line: &[u8]) -> Result<Option<Reply>, ReplyError> {
        let line = line
            .strip_suffix(b"\n")
            .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
            .unwrap_or(line);
        if line.len() > MAX_REPLY_LINE {
            return Err(ReplyError::LineTooLong);
        }
        if line.len() < 3 || !line[..3].iter().all(u8::is_ascii_digit) {
            return Err(ReplyError::Malformed);
        }
        let code = u16::from(line[0] - b'0') * 100
            + u16::from(line[1] - b'0') * 10
            + u16::from(line[2] - b'0');
        if !(200..=599).contains(&code) {
            return Err(ReplyError::Malformed);
        }
        let (last, text) = match line.get(3) {
            None => (true, &line[3..3]),
            Some(b' ') => (true, &line[4..]),
            Some(b'-') => (false, &line[4..]),
            Some(_) => return Err(ReplyError::Malformed),
        };
        if text.iter().any(|&b| b == b'\r' || b == b'\n' || b == 0) {
            return Err(ReplyError::Malformed);
        }
        if self.code.is_some_and(|c| c != code) {
            return Err(ReplyError::CodeMismatch);
        }
        if self.lines.len() >= MAX_REPLY_LINES {
            return Err(ReplyError::TooManyLines);
        }
        self.code = Some(code);
        self.lines.push(String::from_utf8_lossy(text).into_owned());
        if last {
            let lines = std::mem::take(&mut self.lines);
            self.code = None;
            return Ok(Some(Reply { code, lines }));
        }
        Ok(None)
    }
}

/// Parses a buffer holding whole replies (tests and fuzzing).
pub fn parse_replies(data: &[u8]) -> Result<Vec<Reply>, ReplyError> {
    let mut parser = ReplyParser::new();
    let mut replies = Vec::new();
    for line in data.split_inclusive(|&b| b == b'\n') {
        if let Some(reply) = parser.push(line)? {
            replies.push(reply);
        }
    }
    Ok(replies)
}

/// SMTP service extensions announced in the EHLO reply (RFC 5321 §4.1.1.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extensions {
    pub starttls: bool,
    pub auth_plain: bool,
    pub auth_login: bool,
    /// Largest message the server accepts (`SIZE`), 0 = no limit given.
    pub size: Option<u64>,
}

impl Extensions {
    /// Reads the extension lines of an EHLO reply (all but the first).
    pub fn from_ehlo(reply: &Reply) -> Self {
        let mut ext = Self::default();
        for line in reply.lines.iter().skip(1) {
            let mut words = line.split_ascii_whitespace();
            let Some(keyword) = words.next() else {
                continue;
            };
            match keyword.to_ascii_uppercase().as_str() {
                "STARTTLS" => ext.starttls = true,
                "AUTH" => {
                    for mechanism in words {
                        match mechanism.to_ascii_uppercase().as_str() {
                            "PLAIN" => ext.auth_plain = true,
                            "LOGIN" => ext.auth_login = true,
                            _ => {}
                        }
                    }
                }
                "SIZE" => ext.size = words.next().and_then(|v| v.parse().ok()),
                _ => {}
            }
        }
        ext
    }
}

/// Validates and normalises an e-mail address (callers trim user input):
/// ASCII only, `local@domain`
/// with a dot-atom local part and a host name, at most
/// [`MAX_ADDRESS_LEN`] characters, returned in lower case. The same rules
/// as for scheduling addresses; nothing that could break out of an SMTP
/// command or a header gets through.
pub fn normalize_address(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let (local, domain) = lower.split_once('@')?;
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~.".contains(&b));
    let domain_ok = domain.len() <= 253
        && domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    (local_ok && domain_ok && lower.len() <= MAX_ADDRESS_LEN).then_some(lower)
}

/// The text with control characters (including CR, LF and TAB) replaced
/// by spaces, runs of whitespace collapsed and trimmed: safe for a single
/// header line before encoding.
pub fn single_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_control() || c.is_whitespace() || is_invisible(c) {
            space = !out.is_empty();
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    out
}

/// Bidirectional controls and other invisible characters that could
/// disguise a subject.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

/// `text` cut to `max` characters, with "…" if it was longer.
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(BASE64[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Whether `text` can stand in a header as it is: printable ASCII and
/// space, no encoded-word lookalike.
fn plain_header_text(text: &str) -> bool {
    text.bytes().all(|b| (0x20..0x7f).contains(&b)) && !text.contains("=?")
}

/// `text` (a single line, see [`single_line`]) as header content: as it is
/// if plain ASCII, else as RFC 2047 `B` encoded-words of at most 75
/// characters, folded onto continuation lines. `prefix_len` is the length
/// of the header name and colon before it.
pub fn encode_header_text(text: &str, prefix_len: usize) -> String {
    if plain_header_text(text) && prefix_len + text.len() <= MAX_LINE {
        return text.to_owned();
    }
    encoded_words(text)
}

/// RFC 2047 `B` encoded-words of `text`, folded onto continuation lines.
fn encoded_words(text: &str) -> String {
    // 39 bytes become 52 base64 characters: "=?UTF-8?B?" + 52 + "?=" = 64,
    // so even the first line with "Subject: " stays below 76 characters.
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in text.chars() {
        if chunk.len() + c.len_utf8() > 39 {
            words.push(format!("=?UTF-8?B?{}?=", base64(chunk.as_bytes())));
            chunk.clear();
        }
        chunk.push(c);
    }
    if !chunk.is_empty() || words.is_empty() {
        words.push(format!("=?UTF-8?B?{}?=", base64(chunk.as_bytes())));
    }
    words.join("\r\n ")
}

/// A display name for `From:`: a quoted string if plain ASCII, else an
/// encoded-word.
fn encode_display_name(name: &str) -> String {
    let name = single_line(name);
    if plain_header_text(&name) && !name.contains(['"', '\\']) {
        format!("\"{name}\"")
    } else {
        encoded_words(&name)
    }
}

/// Quoted-printable (RFC 2045 §6.7) of UTF-8 text; `\n`, `\r\n` and `\r`
/// become CRLF. Lines stay below 77 characters, and a dot at the start of
/// a line is encoded too, so the body never needs dot-stuffing.
pub fn quoted_printable(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len() + normalized.len() / 8);
    for (index, line) in normalized.split('\n').enumerate() {
        if index > 0 {
            out.push_str("\r\n");
        }
        let bytes = line.as_bytes();
        let mut width = 0;
        for (i, &b) in bytes.iter().enumerate() {
            let last = i + 1 == bytes.len();
            let plain = match b {
                b'=' => false,
                b' ' | b'\t' => !last,
                33..=126 => true,
                _ => false,
            };
            let mut literal = plain && !(b == b'.' && width == 0);
            let piece_len = if literal { 1 } else { 3 };
            // Room for the piece and a soft break "=".
            if width + piece_len > 75 {
                out.push_str("=\r\n");
                width = 0;
                literal = plain && b != b'.';
            }
            if literal {
                out.push(char::from(b));
                width += 1;
            } else {
                let _ = write!(out, "={b:02X}");
                width += 3;
            }
        }
    }
    out
}

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Calendar date of a day number since 1970-01-01 (H. Hinnant's algorithm).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

/// Short English weekday (`Mon`) and month (`Jan`) names.
pub fn weekday_name(days: i64) -> &'static str {
    WEEKDAYS[usize::try_from((days + 3).rem_euclid(7)).unwrap_or(0)]
}

pub fn month_name(month: u32) -> &'static str {
    MONTHS[usize::try_from(month.clamp(1, 12) - 1).unwrap_or(0)]
}

/// RFC 5322 date of a Unix time in UTC: `Fri, 23 Oct 2026 12:00:00 +0000`.
pub fn rfc5322_date(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{}, {day:02} {} {year:04} {:02}:{:02}:{:02} +0000",
        weekday_name(days),
        month_name(month),
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// Why a message cannot be built or sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MailError {
    #[error("invalid e-mail address")]
    InvalidAddress,
    #[error("invalid message id")]
    InvalidMessageId,
    #[error("line too long")]
    LineTooLong,
    #[error("bare CR or LF, or NUL, in the message")]
    BadLineEnding,
    #[error("malformed header")]
    BadHeader,
}

/// A plain-text message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message<'a> {
    /// Display name of the sender, e.g. `CalCraft`.
    pub from_name: &'a str,
    pub from: &'a str,
    pub to: &'a str,
    /// Any text; made single-line, cut to [`MAX_SUBJECT_CHARS`] and
    /// encoded as needed.
    pub subject: &'a str,
    /// UTF-8 text with `\n` line ends.
    pub body: &'a str,
    /// Unix time for `Date:`.
    pub date: i64,
    /// Unique left part of `Message-ID` (letters, digits, `.`, `-`, `_`);
    /// the right part is the sender's domain.
    pub id: &'a str,
}

impl Message<'_> {
    /// The message as sent (CRLF line ends), checked by [`check_message`].
    pub fn render(&self) -> Result<Vec<u8>, MailError> {
        let from = normalize_address(self.from).ok_or(MailError::InvalidAddress)?;
        let to = normalize_address(self.to).ok_or(MailError::InvalidAddress)?;
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(MailError::InvalidMessageId);
        }
        let domain = from.rsplit_once('@').map_or("localhost", |(_, d)| d);
        let subject = truncate_chars(&single_line(self.subject), MAX_SUBJECT_CHARS);
        let mut out = String::with_capacity(self.body.len() * 2 + 512);
        let _ = write!(out, "Date: {}\r\n", rfc5322_date(self.date));
        let _ = write!(
            out,
            "From: {} <{from}>\r\n",
            encode_display_name(self.from_name)
        );
        let _ = write!(out, "To: <{to}>\r\n");
        let _ = write!(out, "Subject: {}\r\n", encode_header_text(&subject, 9));
        let _ = write!(out, "Message-ID: <{}@{domain}>\r\n", self.id);
        out.push_str("MIME-Version: 1.0\r\n");
        out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
        out.push_str("Content-Transfer-Encoding: quoted-printable\r\n");
        out.push_str("Auto-Submitted: auto-generated\r\n");
        out.push_str("X-Auto-Response-Suppress: All\r\n");
        out.push_str("\r\n");
        out.push_str(&quoted_printable(self.body));
        out.push_str("\r\n");
        let bytes = out.into_bytes();
        check_message(&bytes)?;
        Ok(bytes)
    }
}

/// Checks a message before it is sent: CRLF line ends only, no NUL, lines
/// of at most [`MAX_LINE`] octets, and a header section of `Name: value`
/// lines (or folded continuations) ended by an empty line.
pub fn check_message(message: &[u8]) -> Result<(), MailError> {
    let mut in_headers = true;
    let mut first = true;
    let mut rest = message;
    while !rest.is_empty() {
        let end = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or(MailError::BadLineEnding)?;
        let line = &rest[..end];
        rest = &rest[end + 2..];
        if line.iter().any(|&b| b == b'\r' || b == b'\n' || b == 0) {
            return Err(MailError::BadLineEnding);
        }
        if line.len() > MAX_LINE {
            return Err(MailError::LineTooLong);
        }
        if in_headers {
            if line.is_empty() {
                if first {
                    return Err(MailError::BadHeader);
                }
                in_headers = false;
                continue;
            }
            let continuation = line[0] == b' ' || line[0] == b'\t';
            if continuation {
                if first {
                    return Err(MailError::BadHeader);
                }
            } else {
                let colon = line
                    .iter()
                    .position(|&b| b == b':')
                    .ok_or(MailError::BadHeader)?;
                if colon == 0 || !line[..colon].iter().all(|&b| (33..=126).contains(&b)) {
                    return Err(MailError::BadHeader);
                }
            }
            first = false;
        }
    }
    if in_headers {
        return Err(MailError::BadHeader);
    }
    Ok(())
}

/// The message as transmitted after `DATA`: lines starting with `.` get a
/// second one (RFC 5321 §4.5.2), followed by the end marker `.` CRLF. The
/// message must pass [`check_message`]'s line rules.
pub fn dot_stuff(message: &[u8]) -> Result<Vec<u8>, MailError> {
    if message.contains(&0) {
        return Err(MailError::BadLineEnding);
    }
    let mut out = Vec::with_capacity(message.len() + message.len() / 64 + 5);
    let mut rest = message;
    while !rest.is_empty() {
        let end = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or(MailError::BadLineEnding)?;
        let line = &rest[..end];
        if line.iter().any(|&b| b == b'\r' || b == b'\n') {
            return Err(MailError::BadLineEnding);
        }
        if line.len() > MAX_LINE {
            return Err(MailError::LineTooLong);
        }
        if line.first() == Some(&b'.') {
            out.push(b'.');
        }
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
        rest = &rest[end + 2..];
    }
    out.extend_from_slice(b".\r\n");
    Ok(out)
}

#[cfg(test)]
mod tests;
