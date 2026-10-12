//! Which hyperlinks may leave the workbook. A workbook's hyperlink targets are hostile input: a
//! received `.xlsx` can link a cell to `file:///C:/Windows/System32/calc.exe`, a network share
//! or a custom protocol handler, and following those hands them to the operating system.

/// Schemes a link may use outside the workbook: the web and email.
pub const ALLOWED_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

/// Whether a hyperlink target may be handed to the operating system: an http, https or mailto
/// URL written plainly (no spaces or control characters before or inside the scheme). Paths,
/// `file:`, UNC shares (`\\server\share`), `javascript:` and custom schemes are refused. Strict
/// on purpose: what is handed on must be exactly what was checked.
pub fn is_followable_link(url: &str) -> bool {
    if url.is_empty() || url.chars().any(char::is_control) || url.starts_with(char::is_whitespace) {
        return false;
    }
    let Some((scheme, _)) = url.split_once(':') else { return false };
    let valid = scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid && ALLOWED_SCHEMES.iter().any(|s| scheme.eq_ignore_ascii_case(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_and_email_links_leave_the_workbook() {
        for ok in ["https://example.com/a?b#c", "http://example.com", "HTTPS://EXAMPLE.COM", "mailto:someone@example.com"] {
            assert!(is_followable_link(ok), "{ok}");
        }
        for bad in [
            "file:///C:/Windows/System32/calc.exe",
            "file://attacker/share/payload.exe",
            "\\\\attacker\\share\\payload.exe",
            "C:\\Windows\\System32\\calc.exe",
            "/usr/bin/xterm",
            "javascript:alert(1)",
            "vbscript:msgbox(1)",
            "ms-msdt:/id PCWDiagnostic",
            "search-ms://query=x",
            "smb://host/share",
            " https://example.com",
            "\thttps://example.com",
            "ht\ttps://example.com",
            "https\u{0}://example.com",
            "",
            "example.com",
            "Sheet2!A1",
        ] {
            assert!(!is_followable_link(bad), "{bad:?}");
        }
    }
}
