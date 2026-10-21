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
mod tests {
    use super::*;

    #[test]
    fn parses_depth() {
        assert_eq!(Depth::parse(None), Ok(Depth::Infinity));
        assert_eq!(Depth::parse(Some(b"0")), Ok(Depth::Zero));
        assert_eq!(Depth::parse(Some(b" 1 ")), Ok(Depth::One));
        assert_eq!(Depth::parse(Some(b"Infinity")), Ok(Depth::Infinity));
        assert!(Depth::parse(Some(b"2")).is_err());
        assert!(Depth::parse(Some(b"")).is_err());
    }
}
