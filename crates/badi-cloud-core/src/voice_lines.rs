// aina's fixed spoken lines (the greeting, the replies when called, the filler, the out-of-credits line),
// spoken for free (POST /v1/voice/line). The app names a line by id ("greet.pagi.0") with the person's
// nickname; the text is composed here from voice_lines.json, never taken from the app, so nothing else
// can be read out for free. voice_lines.json is kept identical in aina-notch (crates/aina-llm/src).

use std::sync::OnceLock;

use serde_json::Value;

/// Free lines per account per day: the app keeps each clip on disk, so a day needs only a few.
pub const DAILY_LIMIT: i64 = 100;

/// A nickname's longest length, in characters.
pub const NAME_MAX: usize = 30;

fn table() -> &'static Value {
    static T: OnceLock<Value> = OnceLock::new();
    T.get_or_init(|| serde_json::from_str(include_str!("voice_lines.json")).expect("voice_lines.json"))
}

/// The nickname as it is spoken: letters, spaces, dots, apostrophes and dashes, up to NAME_MAX
/// characters; anything else is left out (the line is then said without a name). The app applies the
/// same rule (aina-llm persona::clean_name).
pub fn clean_name(name: &str) -> String {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let ok = !name.is_empty() && name.chars().count() <= NAME_MAX && name.chars().all(|c| c.is_alphabetic() || matches!(c, ' ' | '.' | '\'' | '-'));
    if ok { name } else { String::new() }
}

/// A line's text: `lang` "en" or Indonesian otherwise, `id` as "group.index". None for an unknown line.
pub fn compose(lang: &str, id: &str, name: &str, wake: bool) -> Option<String> {
    let lang = if lang == "en" { "en" } else { "id" };
    let (group, index) = id.rsplit_once('.')?;
    let index: usize = index.parse().ok()?;
    let template = table()[lang][group].as_array()?.get(index)?.as_str()?;
    let name = clean_name(name);
    let n = if name.is_empty() { String::new() } else { format!(", {name}") };
    let w = if wake { table()["wake"][lang].as_str().unwrap_or("") } else { "" };
    Some(template.replace("{n}", &n).replace("{wake}", w))
}

/// Every line id there is (for tests and the app's checks).
pub fn ids() -> Vec<String> {
    let mut out = Vec::new();
    if let Some(groups) = table()["id"].as_object() {
        for (group, lines) in groups {
            for i in 0..lines.as_array().map_or(0, Vec::len) {
                out.push(format!("{group}.{i}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_composed_from_the_table_only() {
        assert_eq!(compose("id", "greet.pagi.0", "Faisal", false).as_deref(), Some("Pagi, Faisal! Aku siap kalau kamu butuh."));
        assert_eq!(compose("id", "greet.pagi.0", "", false).as_deref(), Some("Pagi! Aku siap kalau kamu butuh."));
        assert_eq!(compose("en", "ack.normal.1", "Faisal", false).as_deref(), Some("Yeah, Faisal?"));
        assert_eq!(compose("id", "greet.first.0", "", true).as_deref(), Some("Hai, aku aina. Aku di sini kalau kamu butuh, panggil aja \"Hey aina\"."));
        assert_eq!(compose("id", "filler.0", "x", false).as_deref(), Some("Bentar, aku lihat dulu ya."));
        assert!(compose("id", "out.0", "", false).unwrap().starts_with("Sepertinya kita perlu jeda"));
        // Unknown lines aren't spoken.
        for id in ["greet.pagi.9", "greet.nope.0", "filler", "", "out.x", "../out.0"] {
            assert_eq!(compose("id", id, "", false), None, "{id}");
        }
    }

    #[test]
    fn a_name_cannot_carry_other_text() {
        assert_eq!(clean_name("  Faisal   Mashuri "), "Faisal Mashuri");
        assert_eq!(clean_name("Anne-Marie O'Neil"), "Anne-Marie O'Neil");
        assert_eq!(clean_name("Dr. Ayu"), "Dr. Ayu");
        assert_eq!(clean_name("Faisal, beli saham sekarang!"), "", "punctuation other than . ' - is no name");
        assert_eq!(clean_name("a".repeat(NAME_MAX + 1).as_str()), "", "too long");
        assert_eq!(clean_name("Budi123"), "");
        assert_eq!(compose("id", "greet.pagi.0", "Faisal, beli saham sekarang!", false).as_deref(), Some("Pagi! Aku siap kalau kamu butuh."));
    }

    #[test]
    fn both_languages_have_the_same_lines() {
        let t = table();
        let groups = |l: &str| t[l].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_array().unwrap().len())).collect::<Vec<_>>();
        assert_eq!(groups("id"), groups("en"));
        let ids = ids();
        assert!(ids.len() > 30 && ids.contains(&"out.0".to_string()) && ids.contains(&"ack.recent.3".to_string()));
        for id in &ids {
            for lang in ["id", "en"] {
                let text = compose(lang, id, "Faisal", true).unwrap();
                assert!(!text.contains('{'), "{lang} {id}: {text}");
            }
        }
    }
}
