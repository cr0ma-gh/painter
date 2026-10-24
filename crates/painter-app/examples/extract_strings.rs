//! Collects the interface's texts into `locales/painter.pot` and updates
//! every `locales/*.po` with them (translations kept, new texts added with an
//! empty translation, texts no longer used kept as `#~` comments).
//!
//! ```text
//! cargo run -p painter-app --example extract_strings
//! ```
//!
//! Texts are the string literals passed to `tr("…")` in the app, and the
//! ones a `name`, `title`, `label`, `hint` or `tooltip` function returns
//! (tool, panel, mode names…), which the app passes through `tr` when it
//! shows them. Tests are skipped.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use painter_app::po::{parse, quote};

#[derive(Debug, PartialEq)]
enum Token {
    Ident(String),
    Str(String, usize),
    Punct(char),
}

/// Rust source → identifiers, string literals (unescaped, with their line) and punctuation.
fn tokens(src: &str) -> Vec<Token> {
    let c: Vec<char> = src.chars().collect();
    let (mut i, mut line, mut out) = (0, 1, Vec::new());
    while i < c.len() {
        let ch = c[i];
        if ch == '\n' {
            line += 1;
            i += 1;
        } else if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                line += usize::from(c[i] == '\n');
                i += 1;
            }
            i += 2;
        } else if ch == 'r'
            && (c.get(i + 1) == Some(&'"') || (c.get(i + 1) == Some(&'#') && c.get(i + 2).is_some_and(|x| *x == '"' || *x == '#')))
        {
            // Raw string r"…", r#"…"#.
            let mut j = i + 1;
            let mut hashes = 0;
            while c.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            j += 1;
            let start_line = line;
            let mut s = String::new();
            while j < c.len() {
                if c[j] == '"' && (0..hashes).all(|k| c.get(j + 1 + k) == Some(&'#')) {
                    j += 1 + hashes;
                    break;
                }
                line += usize::from(c[j] == '\n');
                s.push(c[j]);
                j += 1;
            }
            out.push(Token::Str(s, start_line));
            i = j;
        } else if ch == '"' {
            let start_line = line;
            let mut s = String::new();
            let mut j = i + 1;
            while j < c.len() && c[j] != '"' {
                if c[j] == '\\' {
                    j += 1;
                    match c.get(j) {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('r') => s.push('\r'),
                        Some('0') => s.push('\0'),
                        Some('\n') => {
                            // A line continuation: the newline and the next line's indent go.
                            line += 1;
                            while c.get(j + 1).is_some_and(|x| x.is_whitespace()) {
                                line += usize::from(c[j + 1] == '\n');
                                j += 1;
                            }
                        }
                        Some('u') => {
                            let close = (j..c.len()).find(|&k| c[k] == '}').unwrap_or(j);
                            let hex: String = c[j + 2..close].iter().collect();
                            if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                                s.push(ch);
                            }
                            j = close;
                        }
                        Some(&o) => s.push(o),
                        None => {}
                    }
                } else {
                    line += usize::from(c[j] == '\n');
                    s.push(c[j]);
                }
                j += 1;
            }
            out.push(Token::Str(s, start_line));
            i = j + 1;
        } else if ch == '\'' {
            // A char literal ('x', '\n', '"') or a lifetime ('static).
            if c.get(i + 1) == Some(&'\\') {
                let close = (i + 2..c.len()).find(|&k| c[k] == '\'').unwrap_or(i + 2);
                i = close + 1;
            } else if c.get(i + 2) == Some(&'\'') {
                i += 3;
            } else {
                i += 1;
            }
        } else if ch.is_alphabetic() || ch == '_' {
            let start = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Token::Ident(c[start..i].iter().collect()));
        } else {
            if !ch.is_whitespace() {
                out.push(Token::Punct(ch));
            }
            i += 1;
        }
    }
    out
}

/// The index just past the brace block starting at `open` (a `{`).
fn block_end(t: &[Token], open: usize) -> usize {
    let mut depth = 0;
    for (k, tok) in t.iter().enumerate().skip(open) {
        match tok {
            Token::Punct('{') => depth += 1,
            Token::Punct('}') => {
                depth -= 1;
                if depth == 0 {
                    return k + 1;
                }
            }
            _ => {}
        }
    }
    t.len()
}

/// The texts of one file: (text, line).
fn texts_of(src: &str) -> Vec<(String, usize)> {
    let t = tokens(src);
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        match (&t[i], t.get(i + 1), t.get(i + 2)) {
            // #[cfg(test)] mod tests { … } is left out.
            (Token::Ident(m), Some(Token::Ident(n)), Some(Token::Punct('{'))) if m == "mod" && n == "tests" => {
                i = block_end(&t, i + 2);
                continue;
            }
            (Token::Ident(f), Some(Token::Punct('(')), Some(Token::Str(s, line))) if f == "tr" => out.push((s.clone(), *line)),
            // Helpers that translate their label themselves: every text argument.
            (Token::Ident(f), Some(Token::Punct('(')), _)
                if ["prop_slider", "prop_percent", "section", "group_header", "tr_str"].contains(&f.as_str()) =>
            {
                let mut depth = 0;
                for tok in &t[i + 1..] {
                    match tok {
                        Token::Punct('(') => depth += 1,
                        Token::Punct(')') => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        Token::Str(s, line) if depth == 1 => out.push((s.clone(), *line)),
                        _ => {}
                    }
                }
            }
            // The command palette's table: ("id", "Label", C::Category, …).
            (Token::Punct('('), Some(Token::Str(_, _)), Some(Token::Punct(','))) if matches!((t.get(i + 3), t.get(i + 4), t.get(i + 5)), (Some(Token::Str(_, _)), Some(Token::Punct(',')), Some(Token::Ident(c))) if c == "C") => {
                if let Some(Token::Str(label, line)) = t.get(i + 3) {
                    out.push((label.clone(), *line));
                }
            }
            (Token::Ident(f), Some(Token::Ident(n)), _)
                if f == "fn" && ["name", "title", "label", "hint", "tooltip"].contains(&n.as_str()) =>
            {
                // Only functions that return text.
                let open = (i..t.len()).find(|&k| t[k] == Token::Punct('{')).unwrap_or(t.len());
                let returns_str = t[i..open].iter().any(|x| *x == Token::Ident("str".into()));
                if returns_str {
                    let end = block_end(&t, open);
                    for tok in &t[open..end] {
                        if let Token::Str(s, line) = tok {
                            out.push((s.clone(), *line));
                        }
                    }
                    i = end;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    // Nothing to translate in symbols, numbers or empty texts.
    out.retain(|(s, _)| s.chars().any(|c| c.is_alphabetic()));
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = root.canonicalize().unwrap_or(root);
    let mut files = Vec::new();
    for dir in ["crates/painter-app/src", "crates/painter-core/src", "crates/ui-workspace/src"] {
        rust_files(&root.join(dir), &mut files);
    }
    // Text → where it's used, in the order first seen.
    let mut order: Vec<String> = Vec::new();
    let mut refs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        let rel = f.strip_prefix(&root).unwrap_or(f).to_string_lossy().replace('\\', "/");
        for (text, line) in texts_of(&src) {
            if !refs.contains_key(&text) {
                order.push(text.clone());
            }
            refs.entry(text).or_default().push(format!("{rel}:{line}"));
        }
    }
    let entry = |text: &str, translation: &str| {
        let where_: Vec<&String> = refs[text].iter().take(3).collect();
        let refs_line = where_.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
        format!("#: {refs_line}\nmsgid {}\nmsgstr {}\n", quote(text), quote(translation))
    };
    let locales = root.join("locales");
    let _ = std::fs::create_dir_all(&locales);
    let header = "msgid \"\"\nmsgstr \"\"\n\"Project-Id-Version: Painter\\n\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\"Content-Transfer-Encoding: 8bit\\n\"\n";
    let mut pot = String::from(header);
    pot.push('\n');
    for text in &order {
        pot.push_str(&entry(text, ""));
        pot.push('\n');
    }
    std::fs::write(locales.join("painter.pot"), &pot).expect("write painter.pot");
    println!("{} texts in locales/painter.pot", order.len());

    // Each language: its header, every text (translated or not), old ones kept as comments.
    let Ok(dir) = std::fs::read_dir(&locales) else { return };
    for po_path in dir.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "po")) {
        let old = std::fs::read_to_string(&po_path).unwrap_or_default();
        let pairs = parse(&old);
        let head = pairs.iter().find(|(id, _)| id.is_empty()).map(|(_, s)| s.clone()).unwrap_or_default();
        let known: BTreeMap<&str, &str> = pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let mut out = "msgid \"\"\nmsgstr \"\"\n".to_string();
        for line in head.lines() {
            out.push_str(&quote(&format!("{line}\n")));
            out.push('\n');
        }
        out.push('\n');
        let mut missing = 0;
        for text in &order {
            let t = known.get(text.as_str()).copied().unwrap_or("");
            missing += usize::from(t.is_empty());
            out.push_str(&entry(text, t));
            out.push('\n');
        }
        for (id, s) in &pairs {
            if !id.is_empty() && !refs.contains_key(id) && !s.is_empty() {
                out.push_str(&format!("#~ msgid {}\n#~ msgstr {}\n\n", quote(id), quote(s)));
            }
        }
        std::fs::write(&po_path, out).expect("write .po");
        println!("{}: {} of {} translated", po_path.file_name().unwrap().to_string_lossy(), order.len() - missing, order.len());
    }
}
