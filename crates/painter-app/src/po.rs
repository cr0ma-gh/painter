//! Reading and writing gettext `.po` files (the interface's translations,
//! see `i18n`), shared with `examples/extract_strings.rs`.

/// The `msgid` / `msgstr` pairs of a `.po` file (fuzzy entries left out).
pub fn parse(po: &str) -> Vec<(String, String)> {
    #[derive(PartialEq)]
    enum In {
        None,
        Id,
        Str,
    }
    let mut out = Vec::new();
    let (mut id, mut s, mut fuzzy, mut at) = (String::new(), String::new(), false, In::None);
    let mut flush = |id: &mut String, s: &mut String, fuzzy: &mut bool, at: &mut In| {
        if *at == In::Str && !*fuzzy {
            out.push((std::mem::take(id), std::mem::take(s)));
        }
        id.clear();
        s.clear();
        *fuzzy = false;
        *at = In::None;
    };
    for line in po.lines() {
        let line = line.trim();
        if line.is_empty() {
            flush(&mut id, &mut s, &mut fuzzy, &mut at);
        } else if let Some(flags) = line.strip_prefix("#,") {
            fuzzy = flags.contains("fuzzy");
        } else if line.starts_with('#') {
        } else if let Some(rest) = line.strip_prefix("msgid ") {
            if at == In::Str {
                flush(&mut id, &mut s, &mut fuzzy, &mut at);
            }
            id = unquote(rest);
            at = In::Id;
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            s = unquote(rest);
            at = In::Str;
        } else if line.starts_with('"') {
            match at {
                In::Id => id.push_str(&unquote(line)),
                In::Str => s.push_str(&unquote(line)),
                In::None => {}
            }
        }
    }
    flush(&mut id, &mut s, &mut fuzzy, &mut at);
    out
}

/// A quoted `.po` string's text.
pub fn unquote(q: &str) -> String {
    let inner = q.trim().trim_start_matches('"').strip_suffix('"').unwrap_or("");
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `text` as a quoted `.po` string.
pub fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
