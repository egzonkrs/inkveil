//! The app-wide spellchecker.
//!
//! The dictionary is parsed once on a background thread at startup. Until it is ready no words are
//! underlined. A generation counter tells editors when results may have changed (dictionary
//! loaded, word added, personal list replaced or cleared).

use std::{
    ops::Range,
    sync::{
        Mutex, OnceLock, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use inkveil_spell::SpellChecker;

static CHECKER: OnceLock<RwLock<SpellChecker>> = OnceLock::new();
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Personal words set before the dictionary finished loading.
static PENDING_PERSONAL: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// Loads the dictionary in the background.
pub fn start_loading() {
    thread::Builder::new()
        .name("inkveil-spell".into())
        .spawn(|| {
            let mut checker = SpellChecker::en_us();
            // Holding the lock while publishing means `set_personal` can't slip in between.
            let mut pending = PENDING_PERSONAL
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(words) = pending.take() {
                checker.set_personal(words);
            }
            let _ = CHECKER.set(RwLock::new(checker));
            drop(pending);
            bump();
        })
        .ok();
}

/// 0 until the dictionary is loaded; changes whenever results may change.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

fn bump() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

fn read<R>(f: impl FnOnce(&SpellChecker) -> R) -> Option<R> {
    CHECKER.get()?.read().ok().map(|c| f(&c))
}

fn write(f: impl FnOnce(&mut SpellChecker)) {
    if let Some(lock) = CHECKER.get()
        && let Ok(mut checker) = lock.write()
    {
        f(&mut checker);
        bump();
    }
}

pub fn misspelled(text: &str) -> Vec<Range<usize>> {
    read(|c| c.misspelled(text)).unwrap_or_default()
}

pub fn suggest(word: &str) -> Vec<String> {
    read(|c| c.suggest(word)).unwrap_or_default()
}

pub fn add_personal(word: &str) {
    write(|c| c.add_personal(word));
}

pub fn set_personal(words: Vec<String>) {
    let mut pending = PENDING_PERSONAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if CHECKER.get().is_some() {
        drop(pending);
        write(|c| c.set_personal(words));
    } else {
        *pending = Some(words);
    }
}

pub fn clear_personal() {
    if let Ok(mut pending) = PENDING_PERSONAL.lock() {
        *pending = None;
    }
    write(SpellChecker::clear_personal);
}

pub fn personal_words() -> Vec<String> {
    read(SpellChecker::personal_words).unwrap_or_default()
}
