//! Interface languages. Texts are written in English in the code and passed
//! through [`tr`]; each language is a gettext `.po` file (the format Poedit,
//! Weblate and Crowdin edit) whose `msgid` is the English text and `msgstr`
//! the translation. An empty or missing translation shows the English.
//!
//! Languages come from `locales/*.po` in the source (built in), and from
//! `locales/*.po` in Painter's settings folder, so a translator can try a
//! file without building the app (it replaces a built-in one of the same
//! code). `cargo run -p painter-app --example extract_strings` updates
//! `locales/painter.pot` and every `.po` with the texts in the code; see
//! `locales/README.md`.

use std::collections::HashMap;
use std::sync::RwLock;

/// Built-in translations: (code, file).
const BUILT_IN: &[(&str, &str)] = &[("it", include_str!("../../../locales/it.po"))];

struct Catalog {
    code: String,
    texts: HashMap<&'static str, &'static str>,
}

static CURRENT: RwLock<Option<Catalog>> = RwLock::new(None);

/// `text` in the interface language (as it is when there's no translation).
pub(crate) fn tr(text: &'static str) -> &'static str {
    match CURRENT.read() {
        Ok(guard) => guard.as_ref().and_then(|c| c.texts.get(text).copied()).unwrap_or(text),
        Err(_) => text,
    }
}

/// [`tr`] for a text that isn't a literal (a helper's label, a workspace's name).
pub(crate) fn tr_str(text: &str) -> &str {
    match CURRENT.read() {
        Ok(guard) => guard.as_ref().and_then(|c| c.texts.get(text).copied()).unwrap_or(text),
        Err(_) => text,
    }
}

/// A `.po` file's language name, from its `Language-Team` or `Language` header.
fn language_name(po: &str, code: &str) -> String {
    let header = crate::po::parse(po).into_iter().find(|(id, _)| id.is_empty()).map(|(_, s)| s).unwrap_or_default();
    header
        .lines()
        .find_map(|l| l.strip_prefix("X-Language-Name:").map(|v| v.trim().to_string()))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| code.to_string())
}

/// Every language on offer: (code, name), English first.
pub(crate) fn languages() -> Vec<(String, String)> {
    let mut out = vec![("en".to_string(), "English".to_string())];
    let mut add = |code: String, po: &str| {
        if !out.iter().any(|(c, _)| *c == code) {
            out.push((code.clone(), language_name(po, &code)));
        }
    };
    for (path, po) in user_files() {
        if let Some(code) = path.file_stem().and_then(|s| s.to_str()) {
            add(code.to_string(), &po);
        }
    }
    for (code, po) in BUILT_IN {
        add(code.to_string(), po);
    }
    out
}

fn user_files() -> Vec<(std::path::PathBuf, String)> {
    let Some(dir) = crate::files::app_data_dir().map(|d| d.join("locales")) else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "po"))
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|s| (p, s)))
        .collect()
}

/// The system's language code ("it", "en"…).
pub(crate) fn system_language() -> String {
    #[cfg(windows)]
    {
        let mut buf = [0u16; 85];
        // SAFETY: the buffer is LOCALE_NAME_MAX_LENGTH long, as the call wants.
        let n = unsafe { windows::Win32::Globalization::GetUserDefaultLocaleName(&mut buf) };
        if n > 1 {
            return short_code(&String::from_utf16_lossy(&buf[..n as usize - 1]));
        }
    }
    for var in ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() && v != "C" && v != "POSIX" {
                return short_code(&v);
            }
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("defaults").args(["read", "-g", "AppleLocale"]).output() {
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !v.is_empty() {
            return short_code(&v);
        }
    }
    "en".into()
}

/// "it_IT.UTF-8", "it-IT" → "it".
fn short_code(locale: &str) -> String {
    locale.split(['_', '-', '.', '@']).next().unwrap_or("en").to_ascii_lowercase()
}

/// Uses `code` ("" for the system's); returns the code in use. Unknown codes
/// (or English) show the texts as written.
pub(crate) fn set_language(code: &str) -> String {
    let wanted = if code.is_empty() { system_language() } else { code.to_string() };
    let po = user_files()
        .into_iter()
        .find(|(p, _)| p.file_stem().and_then(|s| s.to_str()) == Some(wanted.as_str()))
        .map(|(_, s)| s)
        .or_else(|| BUILT_IN.iter().find(|(c, _)| *c == wanted).map(|(_, s)| s.to_string()));
    let catalog = po.map(|po| {
        let texts = crate::po::parse(&po)
            .into_iter()
            .filter(|(id, s)| !id.is_empty() && !s.is_empty())
            // Kept for the program's life; a language switch adds at most one catalogue.
            .map(|(id, s)| (&*Box::leak(id.into_boxed_str()), &*Box::leak(s.into_boxed_str())))
            .collect();
        Catalog { code: wanted.clone(), texts }
    });
    let used = catalog.as_ref().map_or_else(|| "en".to_string(), |c| c.code.clone());
    if let Ok(mut g) = CURRENT.write() {
        *g = catalog;
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn po_files_parse() {
        let po = "msgid \"\"\nmsgstr \"X-Language-Name: Italiano\\n\"\n\n#: a.rs\nmsgid \"Open...\"\nmsgstr \"Apri...\"\n\n#, fuzzy\nmsgid \"Save\"\nmsgstr \"Salva?\"\n\nmsgid \"Two\\nlines \\\"x\\\"\"\nmsgstr \"\"\n\"Due \"\n\"righe\"\n";
        let pairs = crate::po::parse(po);
        assert!(pairs.contains(&("Open...".into(), "Apri...".into())));
        assert!(!pairs.iter().any(|(id, _)| id == "Save"), "fuzzy entries are skipped");
        assert!(pairs.contains(&("Two\nlines \"x\"".into(), "Due righe".into())));
        assert_eq!(language_name(po, "it"), "Italiano");
        assert_eq!(crate::po::unquote(&crate::po::quote("a \"b\" \\ c\n")), "a \"b\" \\ c\n");
        assert_eq!(short_code("it_IT.UTF-8"), "it");
        assert_eq!(short_code("pt-BR"), "pt");
    }

    #[test]
    fn built_in_languages_are_complete() {
        // Every text of the template has a translation (run the extractor after changing texts).
        let template: Vec<String> = crate::po::parse(include_str!("../../../locales/painter.pot"))
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| !id.is_empty())
            .collect();
        for (code, po) in BUILT_IN {
            let done: HashMap<String, String> = crate::po::parse(po).into_iter().collect();
            let missing: Vec<&String> = template.iter().filter(|id| done.get(*id).is_none_or(|s| s.is_empty())).collect();
            assert!(missing.is_empty(), "{code}.po misses {} texts, e.g. {:?}", missing.len(), &missing[..missing.len().min(5)]);
        }
    }

    #[test]
    fn italian_is_built_in_and_switches() {
        assert!(languages().iter().any(|(c, n)| c == "it" && n == "Italiano"));
        assert_eq!(set_language("it"), "it");
        assert_eq!(tr("Cancel"), "Annulla");
        assert_eq!(tr("No such text anywhere"), "No such text anywhere");
        assert_eq!(set_language("en"), "en");
        assert_eq!(tr("Cancel"), "Cancel");
    }
}
