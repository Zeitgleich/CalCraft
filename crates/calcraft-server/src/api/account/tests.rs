use super::*;

#[test]
fn recovery_codes_round_trip() {
    let code = "abcdefghijklmnop";
    let shown = format_recovery_code(code);
    assert_eq!(shown, "abcd-efgh-ijkl-mnop");
    assert_eq!(normalize_recovery_code(&shown.to_uppercase()), code);
    assert_eq!(normalize_recovery_code(" abcd efgh-ijkl mnop "), code);
}
