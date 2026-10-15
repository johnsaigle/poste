use mailparse::{parse_mail, ParsedMail};

/// Extract readable text from an .eml: prefer text/plain, fall back to
/// text/html stripped to text. Never fails — malformed mail yields "".
pub fn extract_body(eml: &[u8]) -> String {
    let Ok(mail) = parse_mail(eml) else {
        return String::new();
    };
    let mut plain: Option<String> = None;
    let mut html: Option<String> = None;
    collect(&mail, &mut plain, &mut html);
    plain.unwrap_or_else(|| html.map_or_else(String::new, |h| html2text::from_read(h.as_bytes(), 2000)))
}

fn collect(part: &ParsedMail, plain: &mut Option<String>, html: &mut Option<String>) {
    if part.subparts.is_empty() {
        let mt = part.ctype.mimetype.as_str();
        if mt == "text/plain" && plain.is_none() {
            if let Ok(body) = part.get_body() {
                *plain = Some(body);
            }
        } else if mt == "text/html" && html.is_none()
            && let Ok(body) = part.get_body() {
                *html = Some(body);
            }
        return;
    }
    for sub in &part.subparts {
        collect(sub, plain, html);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_plain_text() {
        let eml = b"From: a@b.c\r\nSubject: t\r\nContent-Type: text/plain\r\n\r\nhello world";
        assert_eq!(extract_body(eml), "hello world");
    }

    #[test]
    fn strips_html() {
        let eml = b"From: a@b.c\r\nSubject: t\r\nContent-Type: text/html\r\n\r\n<p>hello <b>html</b></p>";
        let out = extract_body(eml);
        assert!(out.contains("hello"), "got: {out}");
        assert!(out.contains("html"));
        assert!(!out.contains('<'));
    }

    #[test]
    fn malformed_is_empty_not_panic() {
        assert_eq!(extract_body(b"\x00\xffnot an eml"), "");
    }
}
