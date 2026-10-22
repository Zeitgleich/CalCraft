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
