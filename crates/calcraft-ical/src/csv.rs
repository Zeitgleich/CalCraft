//! A small RFC 4180 CSV reader and writer.
//!
//! The writer neutralises spreadsheet formulas (CWE-1236): a cell starting
//! with `=`, `+`, `-`, `@`, TAB or CR is prefixed with `'`, so spreadsheet
//! applications show it as text instead of evaluating it. The reader
//! removes exactly this prefix again, so exports can be re-imported.

use crate::IcalError;

/// Maximum number of columns per row.
pub const MAX_COLUMNS: usize = 64;
/// Maximum size of a single cell in bytes.
pub const MAX_CELL: usize = 64 * 1024;

fn needs_neutralising(cell: &str) -> bool {
    cell.starts_with(['=', '+', '-', '@', '\t', '\r'])
}

/// Appends one cell, neutralised and quoted as needed.
pub fn push_cell(cell: &str, out: &mut String) {
    let neutral = needs_neutralising(cell);
    let quote = neutral || cell.contains([',', '"', '\r', '\n']) || cell.starts_with(' ');
    if quote {
        out.push('"');
    }
    if neutral {
        out.push('\'');
    }
    for c in cell.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    if quote {
        out.push('"');
    }
}

/// Appends one row terminated by CRLF.
pub fn push_row<S: AsRef<str>>(cells: &[S], out: &mut String) {
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_cell(cell.as_ref(), out);
    }
    out.push_str("\r\n");
}

/// Parses CSV text into rows of cells. Empty lines are skipped.
pub fn parse(text: &str, max_rows: usize) -> Result<Vec<Vec<String>>, IcalError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut chars = text.chars().peekable();
    // Whether the current cell started with a quote.
    let mut quoted = false;
    let mut in_quotes = false;
    let mut cell_started = false;

    let finish_cell = |cell: &mut String, row: &mut Vec<String>, quoted: bool| {
        let mut value = std::mem::take(cell);
        // Remove the formula neutralisation added by the writer.
        if quoted && value.starts_with('\'') && needs_neutralising(&value[1..]) {
            value.remove(0);
        }
        row.push(value);
    };

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cell.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                cell.push(c);
            }
        } else {
            match c {
                '"' if !cell_started => {
                    quoted = true;
                    in_quotes = true;
                    cell_started = true;
                }
                '"' if quoted => {
                    return Err(IcalError::Malformed("CSV: text after closing quote"));
                }
                ',' => {
                    finish_cell(&mut cell, &mut row, quoted);
                    quoted = false;
                    cell_started = false;
                    if row.len() >= MAX_COLUMNS {
                        return Err(IcalError::Limit("CSV: too many columns"));
                    }
                }
                '\r' | '\n' => {
                    if c == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    if cell_started || !row.is_empty() {
                        finish_cell(&mut cell, &mut row, quoted);
                        rows.push(std::mem::take(&mut row));
                        if rows.len() > max_rows {
                            return Err(IcalError::Limit("CSV: too many rows"));
                        }
                    }
                    quoted = false;
                    cell_started = false;
                }
                c if quoted => {
                    if !c.is_whitespace() {
                        return Err(IcalError::Malformed("CSV: text after closing quote"));
                    }
                }
                c => {
                    cell.push(c);
                    cell_started = true;
                }
            }
        }
        if cell.len() > MAX_CELL {
            return Err(IcalError::Limit("CSV: cell too large"));
        }
    }
    if in_quotes {
        return Err(IcalError::Malformed("CSV: unterminated quote"));
    }
    if cell_started || !row.is_empty() {
        finish_cell(&mut cell, &mut row, quoted);
        rows.push(row);
        if rows.len() > max_rows {
            return Err(IcalError::Limit("CSV: too many rows"));
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests;
