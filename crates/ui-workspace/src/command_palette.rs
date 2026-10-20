//! The command palette (F3): everything the app can do, registered by id,
//! found by typing a few letters.
//!
//! The app registers its menu items, tools, filters and brush presets at
//! start (and re-registers dynamic groups such as brush presets when they
//! change); the palette searches labels and keywords with a fuzzy
//! subsequence match and hands back the chosen command's id to run.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandCategory {
    File,
    Edit,
    Select,
    Layer,
    View,
    Tool,
    Brush,
    Filter,
    Workspace,
    Script,
}

impl CommandCategory {
    pub fn label(self) -> &'static str {
        match self {
            CommandCategory::File => "File",
            CommandCategory::Edit => "Edit",
            CommandCategory::Select => "Select",
            CommandCategory::Layer => "Layer",
            CommandCategory::View => "View",
            CommandCategory::Tool => "Tool",
            CommandCategory::Brush => "Brush",
            CommandCategory::Filter => "Filter",
            CommandCategory::Workspace => "Workspace",
            CommandCategory::Script => "Script",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    /// Stable id the app dispatches on, e.g. `"file.save"`, `"tool.fill"`,
    /// `"brush.preset.g-pen"`.
    pub id: String,
    pub label: String,
    pub category: CommandCategory,
    /// Shown beside the label, e.g. `"Ctrl+S"`.
    pub shortcut: Option<String>,
    /// Extra words that find it (synonyms, other apps' names).
    pub keywords: Vec<String>,
    pub enabled: bool,
}

impl Command {
    pub fn new(id: &str, label: &str, category: CommandCategory) -> Self {
        Self { id: id.into(), label: label.into(), category, shortcut: None, keywords: Vec::new(), enabled: true }
    }

    pub fn shortcut(mut self, keys: &str) -> Self {
        self.shortcut = Some(keys.into());
        self
    }

    pub fn keywords(mut self, words: &[&str]) -> Self {
        self.keywords = words.iter().map(|w| w.to_string()).collect();
        self
    }
}

/// A search hit.
#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    /// Index into [`CommandRegistry::commands`].
    pub index: usize,
    pub score: i32,
    /// Byte offsets of the matched characters in the label (for bolding);
    /// empty when only a keyword matched.
    pub highlights: Vec<usize>,
}

/// Commands run recently rank higher; this many are remembered.
const RECENT: usize = 20;

#[derive(Clone, Debug, Default)]
pub struct CommandRegistry {
    commands: Vec<Command>,
    recent: VecDeque<String>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    pub fn get(&self, id: &str) -> Option<&Command> {
        self.commands.iter().find(|c| c.id == id)
    }

    /// Adds a command, replacing one with the same id.
    pub fn register(&mut self, command: Command) {
        match self.commands.iter_mut().find(|c| c.id == command.id) {
            Some(existing) => *existing = command,
            None => self.commands.push(command),
        }
    }

    /// Drops every command whose id starts with `prefix` (to rebuild a
    /// dynamic group such as `"brush.preset."`).
    pub fn unregister_prefix(&mut self, prefix: &str) {
        self.commands.retain(|c| !c.id.starts_with(prefix));
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) {
        if let Some(c) = self.commands.iter_mut().find(|c| c.id == id) {
            c.enabled = enabled;
        }
    }

    /// Records that a command ran, for ranking.
    pub fn note_used(&mut self, id: &str) {
        self.recent.retain(|r| r != id);
        self.recent.push_front(id.to_string());
        self.recent.truncate(RECENT);
    }

    pub fn recent(&self) -> impl Iterator<Item = &str> {
        self.recent.iter().map(String::as_str)
    }

    /// Best matches for `query`, best first. An empty query lists recent
    /// commands, then the rest in registration order.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Match> {
        let query = query.trim();
        let recent_bonus = |id: &str| self.recent.iter().position(|r| r == id).map_or(0, |k| (RECENT - k) as i32 * 2);
        let mut out: Vec<Match> = if query.is_empty() {
            let mut all: Vec<Match> = self
                .commands
                .iter()
                .enumerate()
                .map(|(index, c)| Match { index, score: recent_bonus(&c.id), highlights: Vec::new() })
                .collect();
            all.sort_by_key(|m| std::cmp::Reverse(m.score));
            all
        } else {
            self.commands
                .iter()
                .enumerate()
                .filter_map(|(index, c)| {
                    let label = fuzzy(query, &c.label);
                    let keyword = c.keywords.iter().filter_map(|k| fuzzy(query, k)).map(|(s, _)| s - 10).max();
                    let category = fuzzy(query, c.category.label()).map(|(s, _)| s - 20);
                    let (score, highlights) = match (label, keyword.max(category)) {
                        (Some((s, h)), other) if other.is_none_or(|o| s >= o) => (s, h),
                        (_, Some(o)) => (o, Vec::new()),
                        (Some((s, h)), None) => (s, h),
                        (None, None) => return None,
                    };
                    let disabled = if c.enabled { 0 } else { -50 };
                    Some(Match { index, score: score + recent_bonus(&c.id) + disabled, highlights })
                })
                .collect()
        };
        out.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
        out.truncate(limit);
        out
    }
}

/// Fuzzy subsequence match of `query` in `text`, case-insensitive: every
/// query character must appear in order. Scores favour matches at word
/// starts, consecutive runs and a matching prefix, and penalise gaps and
/// long texts. Returns the score and the matched byte offsets.
pub fn fuzzy(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let lower: Vec<char> = chars.iter().map(|(_, c)| c.to_lowercase().next().unwrap_or(*c)).collect();
    let word_start = |i: usize| {
        i == 0 || {
            let prev = chars[i - 1].1;
            !prev.is_alphanumeric() || (prev.is_lowercase() && chars[i].1.is_uppercase())
        }
    };

    // Two greedy alignments: one keeps runs of consecutive characters, the
    // other jumps to word starts; the better-scoring one wins.
    let align = |keep_runs: bool| -> Option<Vec<usize>> {
        let mut hits: Vec<usize> = Vec::with_capacity(q.len());
        let mut at = 0;
        for &qc in &q {
            let next = (at..lower.len()).find(|&i| lower[i] == qc)?;
            let continues = keep_runs && hits.last().is_some_and(|&h| h + 1 == next);
            let start = (next..lower.len()).find(|&i| lower[i] == qc && word_start(i));
            // Jump to a word start only if the rest can still match after it.
            let pick = match start {
                Some(s) if s != next && !continues && rest_matches(&lower, s + 1, &q[hits.len() + 1..]) => s,
                _ => next,
            };
            hits.push(pick);
            at = pick + 1;
        }
        Some(hits)
    };
    let score_of = |hits: &[usize]| {
        let mut score = 0i32;
        for (k, &i) in hits.iter().enumerate() {
            score += 10;
            if word_start(i) {
                score += 16;
            }
            if k > 0 && hits[k - 1] + 1 == i {
                score += 12;
            }
            if k > 0 {
                score -= (i - hits[k - 1] - 1).min(10) as i32;
            }
        }
        if hits[0] == 0 {
            score += 24;
        }
        if hits.len() == lower.len() {
            score += 30; // exact
        }
        score - (lower.len() as i32 - hits.len() as i32).min(30) / 3
    };
    let runs = align(true)?;
    let starts = align(false)?;
    let (score, hits) = [(score_of(&runs), runs), (score_of(&starts), starts)].into_iter().max_by_key(|(s, _)| *s)?;
    Some((score, hits.into_iter().map(|i| chars[i].0).collect()))
}
fn rest_matches(lower: &[char], from: usize, rest: &[char]) -> bool {
    let mut at = from;
    for &c in rest {
        match (at..lower.len()).find(|&i| lower[i] == c) {
            Some(i) => at = i + 1,
            None => return false,
        }
    }
    true
}

/// The palette popup's own state: what's typed and which row is picked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaletteState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
}

impl PaletteState {
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Arrow keys: moves the pick, wrapping within `count` results.
    pub fn move_selection(&mut self, delta: i32, count: usize) {
        if count == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as i64 + delta as i64).rem_euclid(count as i64) as usize;
    }

    /// Call when the query text changes: the best hit is picked again.
    pub fn query_changed(&mut self) {
        self.selected = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        r.register(Command::new("file.save", "Save", CommandCategory::File).shortcut("Ctrl+S"));
        r.register(Command::new("file.save_as", "Save As...", CommandCategory::File).shortcut("Ctrl+Shift+S"));
        r.register(Command::new("file.export_png", "Export PNG...", CommandCategory::File));
        r.register(Command::new("layer.new_raster", "New Raster Layer", CommandCategory::Layer));
        r.register(Command::new("tool.fill", "Fill Bucket", CommandCategory::Tool).keywords(&["paint bucket", "flood"]));
        r.register(
            Command::new("view.zen", "Toggle Zen Mode", CommandCategory::View).shortcut("F11").keywords(&["fullscreen", "hide ui"]),
        );
        r.register(Command::new("brush.preset.g-pen", "G-Pen", CommandCategory::Brush));
        r
    }

    fn ids(r: &CommandRegistry, q: &str) -> Vec<String> {
        r.search(q, 10).iter().map(|m| r.commands()[m.index].id.clone()).collect()
    }

    #[test]
    fn fuzzy_finds_and_ranks() {
        let r = registry();
        assert_eq!(ids(&r, "save")[0], "file.save", "exact beats longer");
        assert_eq!(ids(&r, "nrl")[0], "layer.new_raster", "word initials");
        assert_eq!(ids(&r, "bucket")[0], "tool.fill");
        assert_eq!(ids(&r, "flood")[0], "tool.fill", "keywords");
        assert_eq!(ids(&r, "fullscr")[0], "view.zen");
        assert_eq!(ids(&r, "gpen")[0], "brush.preset.g-pen");
        assert!(ids(&r, "xyzzy").is_empty());
        let m = &r.search("exp", 1)[0];
        assert_eq!(m.highlights, vec![0, 1, 2]);
    }

    #[test]
    fn word_starts_are_preferred() {
        // "as" picks the word "As", not the "a" inside "Save".
        let (_, hits) = fuzzy("as", "Save As...").unwrap();
        assert_eq!(hits, vec![5, 6]);
        // Runs win over scattered word starts.
        let (_, hits) = fuzzy("exp", "Export PNG...").unwrap();
        assert_eq!(hits, vec![0, 1, 2]);
        let (_, hits) = fuzzy("ve", "Save").unwrap();
        assert_eq!(hits, vec![2, 3]);
        assert!(fuzzy("zz", "Save").is_none());
    }

    #[test]
    fn recent_and_dynamic_groups() {
        let mut r = registry();
        assert_eq!(ids(&r, "")[0], "file.save");
        r.note_used("tool.fill");
        assert_eq!(ids(&r, "")[0], "tool.fill", "recent first on an empty query");
        r.note_used("file.save_as");
        assert_eq!(ids(&r, "sa")[0], "file.save_as", "recently used wins a close call");

        r.unregister_prefix("brush.preset.");
        assert!(r.get("brush.preset.g-pen").is_none());
        r.register(Command::new("file.save", "Save Project", CommandCategory::File));
        assert_eq!(r.commands().iter().filter(|c| c.id == "file.save").count(), 1, "re-registering replaces");

        let mut p = PaletteState::default();
        p.open();
        p.move_selection(-1, 5);
        assert_eq!(p.selected, 4);
        p.move_selection(2, 5);
        assert_eq!(p.selected, 1);
    }
}
