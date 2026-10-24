# Translating Painter

Painter's interface texts are written in English in the code. Each language
is one gettext `.po` file in this folder: `it.po` is Italian.

- `painter.pot` is the template: every text, untranslated.
- A `.po` file pairs each English text (`msgid`) with its translation
  (`msgstr`). An empty `msgstr` shows the English text.

## Adding a language

1. Copy `painter.pot` to `<code>.po`, where `<code>` is the language's ISO 639
   code (`fr.po`, `de.po`, `ja.po`, `pt.po`…).
2. In its header, set `"Language: <code>\n"` and the name the language menu
   shows, in that language: `"X-Language-Name: Français\n"`.
3. Fill in the `msgstr` lines. Any `.po` editor works: Poedit, Lokalize,
   Gtranslator, or a web tool such as Weblate or Crowdin; a plain text editor
   too.

Keep what the English keeps:
- `...` and `…` at the end of commands that open a window;
- keyboard shortcuts (`Ctrl+T`, `Shift`, `Esc`) as the keyboard shows them;
- `\n` (a line break) and `\"` (a quote mark);
- symbols such as `■`, `▶`, `→`.

Entries marked `#, fuzzy` are ignored until the mark is removed.

## Trying a translation without building Painter

Put the `.po` file in the `locales` folder of Painter's settings folder:

- Windows: `%APPDATA%\Painter\locales\`
- macOS: `~/Library/Application Support/Painter/locales/`
- Linux: `~/.local/share/Painter/locales/`

Then pick the language in View → Language. A file there replaces a built-in
one with the same code.

## Building a language into Painter

Add the file here and one line to `BUILT_IN` in
`crates/painter-app/src/i18n.rs`:

```rust
("fr", include_str!("../../../locales/fr.po")),
```

## When the code's texts change

```text
cargo run -p painter-app --example extract_strings
```

updates `painter.pot` and every `.po` here: translations are kept, new texts
are added with an empty `msgstr`, and texts no longer used are kept at the end
as `#~` comments. It prints how many texts each language has translated.

For developers: show a fixed text with `tr("English text")`; the extractor
finds it. Texts returned by `name`, `title`, `label`, `hint` and `tooltip`
functions (tool, panel and mode names) are found too: show them with
`tr(x.name())`.
