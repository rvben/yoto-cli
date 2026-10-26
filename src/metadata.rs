//! The descriptive fields of a playlist (author, narrator, description,
//! category, age range, language) shown in the Yoto app, set on upload and
//! changed by `content edit`.

use serde_json::{Map, Value, json};

use crate::error::{Error, Result};

/// The categories Yoto's card schema allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Category {
    None,
    Stories,
    Music,
    Radio,
    Podcast,
    Sfx,
    Activities,
    Alarms,
}

impl Category {
    pub fn name(self) -> &'static str {
        match self {
            Category::None => "none",
            Category::Stories => "stories",
            Category::Music => "music",
            Category::Radio => "radio",
            Category::Podcast => "podcast",
            Category::Sfx => "sfx",
            Category::Activities => "activities",
            Category::Alarms => "alarms",
        }
    }
}

/// The oldest age the range accepts; ages are whole years.
pub const MAX_AGE: u8 = 99;

/// The card `metadata` fields that `Metadata` sets.
pub const FIELDS: [&str; 8] = [
    "author",
    "authors",
    "narrators",
    "description",
    "category",
    "minAge",
    "maxAge",
    "languages",
];

/// Fields to set. `None` leaves a field as it is; an empty string removes it,
/// and for the people a list holding only "" removes all of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    /// Authors in order; the first is the one Yoto shows.
    pub authors: Option<Vec<String>>,
    /// Narrators in order.
    pub narrators: Option<Vec<String>>,
    pub description: Option<String>,
    pub category: Option<Category>,
    pub min_age: Option<u8>,
    pub max_age: Option<u8>,
    /// A BCP 47 tag such as `nl` or `en-GB`.
    pub language: Option<String>,
}

impl Metadata {
    pub fn is_empty(&self) -> bool {
        *self == Metadata::default()
    }

    /// Check the fields on their own, before anything is sent.
    pub fn validate(&self) -> Result<()> {
        for (flag, people) in [("--author", &self.authors), ("--read-by", &self.narrators)] {
            if let Some(people) = people
                && people.iter().any(|p| p.trim().is_empty())
                && !names(people).is_empty()
            {
                return Err(Error::invalid(format!(
                    "{flag} \"\" removes everyone and cannot be given beside a name"
                )));
            }
        }
        for (flag, age) in [("--min-age", self.min_age), ("--max-age", self.max_age)] {
            if let Some(a) = age.filter(|a| *a > MAX_AGE) {
                return Err(Error::invalid(format!(
                    "{flag} {a} is not an age from 0 to {MAX_AGE}"
                )));
            }
        }
        if let Some(lang) = self.language.as_deref().filter(|l| !l.is_empty())
            && !is_language_tag(lang)
        {
            return Err(Error::invalid(format!(
                "--language {lang:?} is not a language tag such as nl or en-GB"
            )));
        }
        Ok(())
    }

    /// Write the chosen fields into a card's `metadata` object, then check
    /// the age range as it ends up, so an edit cannot leave min above max.
    pub fn apply(&self, metadata: &mut Map<String, Value>) -> Result<()> {
        // Yoto keeps the people in `authors` and `narrators`. It derives
        // `author` from the first author, and drops a `readBy` beside
        // `narrators`, so a legacy `readBy` goes when the narrator is set.
        // Removing empties a list Yoto keeps and adds none it does not.
        let empty = |metadata: &mut Map<String, Value>, list: &str| {
            if metadata.contains_key(list) {
                metadata.insert(list.into(), json!([]));
            }
        };
        if let Some(authors) = self.authors.as_deref().map(names) {
            match authors.first() {
                None => {
                    metadata.remove("author");
                    empty(metadata, "authors");
                }
                Some(first) => {
                    metadata.insert("author".into(), json!(first));
                    metadata.insert("authors".into(), json!(authors));
                }
            }
        }
        if let Some(narrators) = self.narrators.as_deref().map(names) {
            metadata.remove("readBy");
            if narrators.is_empty() {
                empty(metadata, "narrators");
            } else {
                metadata.insert("narrators".into(), json!(narrators));
            }
        }
        match self.description.as_deref().map(str::trim) {
            None => {}
            Some("") => {
                metadata.remove("description");
            }
            Some(v) => {
                metadata.insert("description".into(), json!(v));
            }
        }
        if let Some(c) = self.category {
            metadata.insert("category".into(), json!(c.name()));
        }
        if let Some(a) = self.min_age {
            metadata.insert("minAge".into(), json!(a));
        }
        if let Some(a) = self.max_age {
            metadata.insert("maxAge".into(), json!(a));
        }
        match self.language.as_deref().map(str::trim) {
            None => {}
            Some("") => {
                metadata.remove("languages");
            }
            Some(l) => {
                metadata.insert("languages".into(), json!([l]));
            }
        }
        let age = |field: &str| metadata.get(field).and_then(Value::as_u64);
        if let (Some(min), Some(max)) = (age("minAge"), age("maxAge"))
            && min > max
        {
            return Err(Error::invalid(format!(
                "the minimum age ({min}) is above the maximum age ({max})"
            )));
        }
        Ok(())
    }

    /// The fields this change removes, under their Yoto names.
    pub fn removals(&self) -> Vec<&'static str> {
        let people = |p: &Option<Vec<String>>| p.as_deref().is_some_and(|p| names(p).is_empty());
        let text = |t: &Option<String>| t.as_deref().is_some_and(|t| t.trim().is_empty());
        [
            ("authors", people(&self.authors)),
            ("narrators", people(&self.narrators)),
            ("description", text(&self.description)),
            ("languages", text(&self.language)),
        ]
        .into_iter()
        .filter_map(|(field, removed)| removed.then_some(field))
        .collect()
    }

    /// The fields that will be set, as they go into the card.
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        // Applying to an empty object cannot fail the age check unless the
        // fields themselves conflict, which `validate` callers report first.
        let _ = self.apply(&mut map);
        Value::Object(map)
    }
}

/// The names as stored: trimmed, without blanks, and each once in the place
/// it was first given, so a list of only "" is empty and removes everyone.
fn names(people: &[String]) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    for name in people.iter().map(|p| p.trim()) {
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// A loose BCP 47 check: letter subtags, then letter or digit subtags, each
/// 1 to 8 long, joined by hyphens.
fn is_language_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let primary = parts.next().unwrap_or_default();
    (2..=8).contains(&primary.len())
        && primary.bytes().all(|b| b.is_ascii_alphabetic())
        && parts.all(|p| (1..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &Metadata, start: Value) -> Result<Value> {
        let Value::Object(mut map) = start else {
            panic!("object")
        };
        m.apply(&mut map)?;
        Ok(Value::Object(map))
    }

    #[test]
    fn sets_given_fields_under_yoto_names_and_keeps_the_rest() {
        let m = Metadata {
            authors: Some(vec![" Ted van Lieshout ".into()]),
            narrators: Some(vec!["Ruben".into()]),
            category: Some(Category::Stories),
            min_age: Some(2),
            max_age: Some(6),
            language: Some("nl".into()),
            ..Metadata::default()
        };
        let out = apply(
            &m,
            json!({"description": "oud", "readBy": "oud", "media": {"duration": 5}}),
        )
        .unwrap();
        // Yoto keeps people in `authors` and `narrators` and derives `author`
        // from the first; a `readBy` it does not store is dropped.
        assert_eq!(
            out,
            json!({
                "author": "Ted van Lieshout",
                "authors": ["Ted van Lieshout"],
                "narrators": ["Ruben"],
                "description": "oud",
                "category": "stories",
                "minAge": 2,
                "maxAge": 6,
                "languages": ["nl"],
                "media": {"duration": 5},
            })
        );
    }

    #[test]
    fn an_empty_value_removes_the_field() {
        let m = Metadata {
            authors: Some(vec![String::new()]),
            narrators: Some(vec![String::new()]),
            description: Some("  ".into()),
            language: Some(String::new()),
            ..Metadata::default()
        };
        let out = apply(
            &m,
            json!({
                "author": "a", "authors": ["a", "b"], "narrators": ["r"], "readBy": "r",
                "description": "d", "languages": ["nl"], "category": "stories"
            }),
        )
        .unwrap();
        assert_eq!(
            out,
            json!({"authors": [], "narrators": [], "category": "stories"})
        );
    }

    #[test]
    fn only_blanks_remove_everyone_and_a_blank_beside_a_name_is_refused() {
        let blanks = Metadata {
            authors: Some(vec![String::new(), "  ".into()]),
            ..Metadata::default()
        };
        blanks.validate().unwrap();
        assert_eq!(blanks.removals(), ["authors"]);
        let mixed = Metadata {
            narrators: Some(vec!["R".into(), " ".into()]),
            ..Metadata::default()
        };
        assert_eq!(mixed.validate().unwrap_err().kind(), "invalid_input");
    }

    #[test]
    fn a_name_given_twice_is_kept_once_in_its_first_place() {
        let m = Metadata {
            authors: Some(vec!["A".into(), "B".into(), " A ".into()]),
            narrators: Some(vec!["R".into(), "R".into()]),
            ..Metadata::default()
        };
        assert_eq!(
            apply(&m, json!({})).unwrap(),
            json!({"author": "A", "authors": ["A", "B"], "narrators": ["R"]})
        );
    }

    #[test]
    fn removing_a_person_that_is_not_there_changes_nothing() {
        let m = Metadata {
            authors: Some(vec![String::new()]),
            narrators: Some(vec![String::new()]),
            ..Metadata::default()
        };
        let start = json!({"category": "stories"});
        assert_eq!(apply(&m, start.clone()).unwrap(), start);
    }

    #[test]
    fn the_resulting_age_range_must_not_be_reversed() {
        let raise_min = Metadata {
            min_age: Some(7),
            ..Metadata::default()
        };
        assert_eq!(
            apply(&raise_min, json!({"maxAge": 6})).unwrap_err().kind(),
            "invalid_input"
        );
        assert!(apply(&raise_min, json!({"maxAge": 7})).is_ok());
        assert!(apply(&raise_min, json!({})).is_ok());
    }

    #[test]
    fn validate_rejects_bad_ages_and_language_tags() {
        let age = Metadata {
            max_age: Some(MAX_AGE + 1),
            ..Metadata::default()
        };
        assert_eq!(age.validate().unwrap_err().kind(), "invalid_input");
        for good in ["nl", "en-GB", "zh-Hant-TW", "es-419"] {
            let m = Metadata {
                language: Some(good.into()),
                ..Metadata::default()
            };
            assert!(m.validate().is_ok(), "{good}");
        }
        for bad in ["n", "nl_NL", "nederlands-", "1a", "en--GB"] {
            let m = Metadata {
                language: Some(bad.into()),
                ..Metadata::default()
            };
            assert_eq!(m.validate().unwrap_err().kind(), "invalid_input", "{bad}");
        }
    }
}
