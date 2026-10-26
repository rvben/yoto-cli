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

/// Fields to set. `None` leaves a field as it is; an empty string removes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    pub author: Option<String>,
    pub read_by: Option<String>,
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
        let text = [
            ("author", &self.author),
            ("readBy", &self.read_by),
            ("description", &self.description),
        ];
        for (field, value) in text {
            match value.as_deref().map(str::trim) {
                None => {}
                Some("") => {
                    metadata.remove(field);
                }
                Some(v) => {
                    metadata.insert(field.into(), json!(v));
                }
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

    /// The fields that will be set, as they go into the card.
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        // Applying to an empty object cannot fail the age check unless the
        // fields themselves conflict, which `validate` callers report first.
        let _ = self.apply(&mut map);
        Value::Object(map)
    }
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
            author: Some(" Ted van Lieshout ".into()),
            read_by: Some("Ruben".into()),
            category: Some(Category::Stories),
            min_age: Some(2),
            max_age: Some(6),
            language: Some("nl".into()),
            ..Metadata::default()
        };
        let out = apply(&m, json!({"description": "oud", "media": {"duration": 5}})).unwrap();
        assert_eq!(
            out,
            json!({
                "author": "Ted van Lieshout",
                "readBy": "Ruben",
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
            author: Some(String::new()),
            description: Some("  ".into()),
            language: Some(String::new()),
            ..Metadata::default()
        };
        let out = apply(
            &m,
            json!({"author": "a", "description": "d", "languages": ["nl"], "readBy": "r"}),
        )
        .unwrap();
        assert_eq!(out, json!({"readBy": "r"}));
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
