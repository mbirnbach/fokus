//! Translations, using Project Fluent (https://projectfluent.org).
//!
//! Every user-facing text has a key in `locales/<lang>.ftl`; code uses
//! `t!("key")` or `t!("key", name = value)`. English and German are built in.
//! More languages can be added without rebuilding: put e.g. `fr.ftl` into
//! `<config folder>/fokus/locales/` and it shows up in Settings. Keys missing
//! from a translation fall back to English.
//!
//! The current language is global (one per app) so texts can be translated
//! anywhere, including the laser connection thread.

use std::path::PathBuf;
use std::sync::RwLock;

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use unic_langid::LanguageIdentifier;

/// Built-in languages: (code, file contents).
const BUILTIN: [(&str, &str); 2] = [("en", include_str!("../locales/en.ftl")), ("de", include_str!("../locales/de.ftl"))];
const FALLBACK: &str = "en";

type Bundle = FluentBundle<FluentResource>;

struct State {
    bundle: Bundle,
    fallback: Bundle,
}

static STATE: RwLock<Option<State>> = RwLock::new(None);

/// A language Fokus can show.
#[derive(Debug, Clone, PartialEq)]
pub struct Language {
    pub code: String,
    /// The language's own name, from its `language-name` message.
    pub name: String,
    pub builtin: bool,
}

/// Folder for community translation files.
pub fn user_locale_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("fokus").join("locales"))
}

fn user_files() -> Vec<(String, String)> {
    let Some(dir) = user_locale_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(String, String)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            (path.extension()? == "ftl").then_some(())?;
            let code = path.file_stem()?.to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).ok()?;
            Some((code, text))
        })
        .collect();
    out.sort();
    out
}

/// The source text for a language: a user file wins over a built-in one,
/// so the community can also improve the built-in translations.
fn source(code: &str) -> Option<String> {
    user_files()
        .into_iter()
        .find(|(c, _)| c == code)
        .map(|(_, t)| t)
        .or_else(|| BUILTIN.iter().find(|(c, _)| *c == code).map(|(_, t)| t.to_string()))
}

fn bundle(code: &str, text: &str) -> Bundle {
    let lang: LanguageIdentifier = code.parse().unwrap_or_else(|_| FALLBACK.parse().unwrap());
    let mut bundle = FluentBundle::new_concurrent(vec![lang]);
    // No invisible Unicode isolation marks around placeables: egui would
    // draw them as boxes.
    bundle.set_use_isolating(false);
    let resource = match FluentResource::try_new(text.to_string()) {
        Ok(r) => r,
        // Keep the messages that did parse.
        Err((r, _errors)) => r,
    };
    // Duplicate keys: the later one wins.
    bundle.add_resource_overriding(resource);
    bundle
}

pub fn available() -> Vec<Language> {
    let mut langs: Vec<Language> = BUILTIN.iter().map(|(code, _)| Language { code: code.to_string(), name: String::new(), builtin: true }).collect();
    for (code, _) in user_files() {
        if !langs.iter().any(|l| l.code == code) {
            langs.push(Language { code, name: String::new(), builtin: false });
        }
    }
    for l in &mut langs {
        let text = source(&l.code).unwrap_or_default();
        let b = bundle(&l.code, &text);
        l.name = message(&b, "language-name", None).unwrap_or_else(|| l.code.clone());
    }
    langs
}

/// The system's language if we have it, else English.
pub fn system_language() -> String {
    let available: Vec<String> = available().into_iter().map(|l| l.code).collect();
    sys_locale::get_locales()
        .filter_map(|loc| {
            let code = loc.split(['-', '_']).next()?.to_lowercase();
            available.contains(&code).then_some(code)
        })
        .next()
        .unwrap_or_else(|| FALLBACK.to_string())
}

/// Switch the language; `None` follows the system.
pub fn set_language(code: Option<&str>) {
    let code = code.map(str::to_string).unwrap_or_else(system_language);
    let text = source(&code).unwrap_or_default();
    let fallback_text = BUILTIN[0].1;
    let state = State { bundle: bundle(&code, &text), fallback: bundle(FALLBACK, fallback_text) };
    *STATE.write().unwrap() = Some(state);
}

fn message(bundle: &Bundle, key: &str, args: Option<&FluentArgs>) -> Option<String> {
    let pattern = bundle.get_message(key)?.value()?;
    let mut errors = Vec::new();
    Some(bundle.format_pattern(pattern, args, &mut errors).into_owned())
}

/// Translate `key`; falls back to English, then to the key itself.
pub fn tr(key: &str, args: Option<&FluentArgs>) -> String {
    if STATE.read().unwrap().is_none() {
        // Tests always run in English, whatever the system language.
        set_language(if cfg!(test) { Some(FALLBACK) } else { None });
    }
    let guard = STATE.read().unwrap();
    let state = guard.as_ref().unwrap();
    message(&state.bundle, key, args).or_else(|| message(&state.fallback, key, args)).unwrap_or_else(|| key.to_string())
}

/// `t!("key")` or `t!("key", name = value, …)`.
#[macro_export]
macro_rules! t {
    ($key:literal) => {
        $crate::i18n::tr($key, None)
    };
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {{
        let mut args = fluent_bundle::FluentArgs::new();
        $( args.set(stringify!($name), $value); )+
        $crate::i18n::tr($key, Some(&args))
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn keys(text: &str) -> BTreeSet<String> {
        text.lines()
            .filter(|l| !l.starts_with([' ', '#', '\t', '.']) && l.contains(" ="))
            .map(|l| l.split(" =").next().unwrap().trim().to_string())
            .collect()
    }

    /// Every `t!("key"…)` in the source, with the files it is used in.
    fn used_keys() -> BTreeSet<String> {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut out = BTreeSet::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let file = std::fs::read_to_string(entry.path()).unwrap();
            // Only program code: tests aren't shown to users.
            let text = file.split("#[cfg(test)]").next().unwrap();
            // The macro as its own word, not the end of format! and friends.
            for (i, _) in text.match_indices("t!(") {
                if text[..i].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                // The key may be on the next line.
                let Some(rest) = text[i + 3..].trim_start().strip_prefix('"') else { continue };
                out.insert(rest.split('"').next().unwrap().to_string());
            }
        }
        out.remove("key");
        out
    }

    #[test]
    fn both_languages_have_the_same_keys() {
        let en = keys(BUILTIN[0].1);
        let de = keys(BUILTIN[1].1);
        assert!(en.len() > 100, "{} keys", en.len());
        let missing_de: Vec<_> = en.difference(&de).collect();
        let extra_de: Vec<_> = de.difference(&en).collect();
        assert!(missing_de.is_empty(), "missing in de.ftl: {missing_de:?}");
        assert!(extra_de.is_empty(), "only in de.ftl: {extra_de:?}");
    }

    #[test]
    fn every_used_key_exists_and_every_key_is_used() {
        let en = keys(BUILTIN[0].1);
        let used = used_keys();
        let missing: Vec<_> = used.difference(&en).collect();
        assert!(missing.is_empty(), "used in code but not in en.ftl: {missing:?}");
        let unused: Vec<_> = en.difference(&used).filter(|k| *k != "language-name").collect();
        assert!(unused.is_empty(), "in en.ftl but not used: {unused:?}");
    }

    #[test]
    fn files_parse_cleanly() {
        for (code, text) in BUILTIN {
            assert!(FluentResource::try_new(text.to_string()).is_ok(), "{code}.ftl has syntax errors");
        }
    }

    #[test]
    fn translation_and_arguments() {
        let de = bundle("de", BUILTIN[1].1);
        let mut args = FluentArgs::new();
        args.set("name", "Unbenannt");
        assert_eq!(message(&de, "title-unsaved", Some(&args)).unwrap(), "Unbenannt (nicht gespeichert) — Fokus");
        let en = bundle("en", BUILTIN[0].1);
        assert_eq!(message(&en, "title-unsaved", Some(&args)).unwrap(), "Unbenannt (unsaved) — Fokus");
    }
}
