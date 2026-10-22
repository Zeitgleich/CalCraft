use super::*;

#[test]
fn writes_quoted_and_neutralised_cells() {
    let mut out = String::new();
    push_row(
        &[
            "plain",
            "a,b",
            "say \"hi\"",
            "line\nbreak",
            "=HYPERLINK(\"http://x\")",
            "+cmd|' /C calc'!A0",
            "-1",
            "@SUM(A1)",
            "\tTAB",
            "\rCR",
            "",
        ],
        &mut out,
    );
    assert_eq!(
        out,
        "plain,\"a,b\",\"say \"\"hi\"\"\",\"line\nbreak\",\"'=HYPERLINK(\"\"http://x\"\")\",\"'+cmd|' /C calc'!A0\",\"'-1\",\"'@SUM(A1)\",\"'\tTAB\",\"'\rCR\",\r\n"
    );
    let rows = parse(&out, 10).unwrap();
    assert_eq!(rows[0][4], "=HYPERLINK(\"http://x\")");
    assert_eq!(rows[0][3], "line\nbreak");
    assert_eq!(rows[0][9], "\rCR");
    assert_eq!(rows[0].len(), 11);
}

#[test]
fn parses_rfc_4180() {
    let rows = parse("\u{feff}a,b\r\n\"x\"\"y\",\"multi\r\nline\"\n\nlast,\n", 10).unwrap();
    assert_eq!(
        rows,
        [
            vec!["a", "b"],
            vec!["x\"y", "multi\r\nline"],
            vec!["last", ""],
        ]
    );
    // Unquoted cells are taken literally, including a leading quote mark.
    assert_eq!(parse("'=1,x", 10).unwrap(), [vec!["'=1", "x"]]);
}

#[test]
fn rejects_malformed_and_oversized_input() {
    assert!(parse("\"open", 10).is_err());
    assert!(parse("\"a\"b,c", 10).is_err());
    assert!(parse(&"a\n".repeat(11), 10).is_err());
    assert!(parse(&",".repeat(MAX_COLUMNS + 1), 10).is_err());
    assert!(parse(&"x".repeat(MAX_CELL + 1), 10).is_err());
}
