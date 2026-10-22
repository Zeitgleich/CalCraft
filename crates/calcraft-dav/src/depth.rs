use crate::DavError;

/// Value of the `Depth` request header (RFC 4918 §10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Zero,
    One,
    Infinity,
}

impl Depth {
    /// Parses the header value. A missing header means `infinity`.
    pub fn parse(value: Option<&[u8]>) -> Result<Self, DavError> {
        match value.map(<[u8]>::trim_ascii) {
            None => Ok(Self::Infinity),
            Some(b"0") => Ok(Self::Zero),
            Some(b"1") => Ok(Self::One),
            Some(v) if v.eq_ignore_ascii_case(b"infinity") => Ok(Self::Infinity),
            Some(_) => Err(DavError::BadRequest("invalid Depth header")),
        }
    }
}

#[cfg(test)]
mod tests;
