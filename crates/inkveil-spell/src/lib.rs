//! Offline spellchecking for Inkveil.
//!
//! Uses [spellbook] (a pure-Rust Hunspell implementation) with the en-US dictionary from
//! [wooorm/dictionaries], compiled into the binary. Text never leaves the process: there is no
//! operating system spellchecker, no download and no network.
//!
//! Words the user adds are kept in a separate personal list, so it can be wiped when the vault
//! locks (the app stores it encrypted in the vault).
//!
//! [wooorm/dictionaries]: https://github.com/wooorm/dictionaries

#![forbid(unsafe_code)]

use std::{collections::HashSet, fmt, ops::Range};

use spellbook::Dictionary;
use unicode_segmentation::UnicodeSegmentation;

const EN_US_AFF: &str = include_str!("../dictionaries/en_US.aff");
const EN_US_DIC: &str = include_str!("../dictionaries/en_US.dic");

/// Most suggestions shown for one word.
const MAX_SUGGESTIONS: usize = 6;

/// A spellchecker for one language plus the user's own words.
pub struct SpellChecker {
    dictionary: Dictionary,
    /// Lowercased words the user added.
    personal: HashSet<String>,
}

impl fmt::Debug for SpellChecker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpellChecker")
            .field("personal_words", &self.personal.len())
            .finish_non_exhaustive()
    }
}

impl SpellChecker {
    /// The bundled US English dictionary. Parsing takes tens of milliseconds, so call this off
    /// the UI thread.
    ///
    /// # Panics
    ///
    /// If the bundled dictionary does not parse, which a unit test rules out.
    pub fn en_us() -> Self {
        Self {
            dictionary: Dictionary::new(EN_US_AFF, EN_US_DIC)
                .expect("the bundled en-US dictionary is valid"),
            personal: HashSet::new(),
        }
    }

    /// Whether `word` is spelled correctly.
    pub fn check(&self, word: &str) -> bool {
        let word = normalize(word);
        self.dictionary.check(&word) || self.personal.contains(&word.to_lowercase())
    }

    /// Byte ranges of misspelled words in `text`.
    ///
    /// Skips anything that is not prose: words containing digits or underscores, single letters,
    /// all-caps acronyms, and whitespace-separated tokens that look like URLs, emails, paths or
    /// domains.
    pub fn misspelled(&self, text: &str) -> Vec<Range<usize>> {
        let skipped = non_prose_spans(text);
        text.split_word_bound_indices()
            .filter(|(start, word)| {
                is_checkable(word)
                    && !skipped.iter().any(|r| r.contains(start))
                    && !self.check(word)
            })
            .map(|(start, word)| start..start + word.len())
            .collect()
    }

    /// Corrections for `word`, best first.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        let mut out = Vec::new();
        self.dictionary.suggest(&normalize(word), &mut out);
        out.truncate(MAX_SUGGESTIONS);
        out
    }

    /// Accepts `word` from now on.
    pub fn add_personal(&mut self, word: &str) {
        let word = normalize(word).to_lowercase();
        if !word.trim().is_empty() {
            self.personal.insert(word);
        }
    }

    /// Replaces the personal word list, e.g. after unlocking.
    pub fn set_personal(&mut self, words: impl IntoIterator<Item = String>) {
        self.personal.clear();
        for word in words {
            self.add_personal(&word);
        }
    }

    /// Forgets the personal words (they may contain names and other private terms).
    pub fn clear_personal(&mut self) {
        self.personal.clear();
    }

    /// The personal words, sorted, for storing.
    pub fn personal_words(&self) -> Vec<String> {
        let mut words: Vec<String> = self.personal.iter().cloned().collect();
        words.sort();
        words
    }
}

/// Typographic apostrophes are checked as ASCII ones.
fn normalize(word: &str) -> String {
    word.replace(['\u{2019}', '\u{02BC}'], "'")
}

fn is_checkable(word: &str) -> bool {
    let letters = word.chars().filter(|c| c.is_alphabetic()).count();
    if letters < 2 {
        return false;
    }
    if word.chars().any(|c| c.is_numeric() || c == '_') {
        return false;
    }
    // Acronyms such as NASA or HTTP.
    let all_caps = word
        .chars()
        .filter(|c| c.is_alphabetic())
        .all(char::is_uppercase);
    !all_caps
}

/// Whitespace-separated tokens that are URLs, emails, file paths or domains.
fn non_prose_spans(text: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices().chain([(text.len(), ' ')]) {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                if looks_technical(&text[s..i]) {
                    spans.push(s..i);
                }
                start = None;
            }
            _ => {}
        }
    }
    spans
}

fn looks_technical(token: &str) -> bool {
    let token = token
        .trim_matches(|c: char| matches!(c, '(' | ')' | '"' | '\'' | ',' | ';' | ':' | '!' | '?'));
    let token = token.strip_suffix('.').unwrap_or(token);
    token.contains("://")
        || token.starts_with("www.")
        || token.contains('@')
        || token.contains('/')
        || token.contains('\\')
        // A dot between letters, as in example.com or file.txt.
        || token
            .char_indices()
            .any(|(i, c)| c == '.' && i > 0 && token[i + 1..].starts_with(char::is_alphanumeric))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(checker: &SpellChecker, text: &str) -> Vec<String> {
        checker
            .misspelled(text)
            .into_iter()
            .map(|r| text[r].to_owned())
            .collect()
    }

    #[test]
    fn finds_misspellings() {
        let checker = SpellChecker::en_us();
        assert_eq!(
            words(&checker, "Thiss is a tset of the spellchecker."),
            ["Thiss", "tset"]
        );
    }

    #[test]
    fn accepts_normal_english() {
        let checker = SpellChecker::en_us();
        let text =
            "Don't worry, it's the color of the neighbor's car. I'm going to Berlin tomorrow.";
        assert_eq!(words(&checker, text), Vec::<String>::new());
        assert!(checker.check("don\u{2019}t"));
    }

    #[test]
    fn skips_technical_tokens() {
        let checker = SpellChecker::en_us();
        let text = "See https://exampel.com/pathh, mail me@exampel.org, open C:\\Usres\\file.txt, \
                    version v2beta, the NASAX team, and x.";
        assert_eq!(words(&checker, text), Vec::<String>::new());
    }

    #[test]
    fn suggestions() {
        let checker = SpellChecker::en_us();
        assert!(checker.suggest("tset").iter().any(|s| s == "test"));
        assert!(checker.suggest("recieve").iter().any(|s| s == "receive"));
        assert!(checker.suggest("recieve").len() <= MAX_SUGGESTIONS);
    }

    #[test]
    fn personal_dictionary() {
        let mut checker = SpellChecker::en_us();
        assert!(!checker.check("Inkveil"));
        checker.add_personal("Inkveil");
        assert!(checker.check("Inkveil"));
        assert!(checker.check("inkveil"));
        assert_eq!(checker.personal_words(), ["inkveil"]);
        checker.clear_personal();
        assert!(!checker.check("Inkveil"));
        checker.set_personal(["Egzon".to_owned()]);
        assert!(checker.check("Egzon"));
    }

    #[test]
    fn offsets_are_byte_ranges() {
        let checker = SpellChecker::en_us();
        // The em dash is three bytes long.
        let text = "Hello — wrold";
        assert_eq!(checker.misspelled(text), vec![10..15]);
    }
}
