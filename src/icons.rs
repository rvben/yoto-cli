//! Chapter icons: the 16x16 pixel art a player shows while a track plays.
//!
//! An `icons.json` assigns icons by track number, with an optional
//! `"default"` for every other track:
//!
//! ```json
//! {"default": "yoto:#aUm9i3ex3qqAMYBv-i-O-pYMKuMJGICtR3Vhf289u2Q",
//!  "1": "icons/olifant.png",
//!  "2": "yoto:#7Wt--qhx9t7PCSC8WBmGKJvlG53Z1uk7bN1dE9wGWJ0"}
//! ```
//!
//! A value is either an image file (PNG, GIF or JPEG, relative to the JSON
//! file) that is uploaded as a custom icon, or a `yoto:#<mediaId>` reference
//! to an icon Yoto already has, such as one from `yoto-cli icons list`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{Error, Result};

/// The file name looked up next to the audio (and in the book folder).
pub const ICONS_FILE: &str = "icons.json";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Icon {
    /// An icon Yoto already stores, by media ID (without the `yoto:#` prefix).
    Yoto(String),
    /// An image to upload as a custom icon.
    File(PathBuf),
}

impl Icon {
    /// Parse a `yoto:#<mediaId>` reference or a file path relative to `base`.
    pub fn parse(spec: &str, base: &Path) -> Result<Icon> {
        let spec = spec.trim();
        if let Some(id) = spec.strip_prefix("yoto:#") {
            if id.len() == 43
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Ok(Icon::Yoto(id.to_string()));
            }
            return Err(Error::invalid(format!(
                "icon {spec:?} is not a Yoto icon reference (yoto:# followed by a 43-character media ID)"
            )));
        }
        if spec.is_empty() {
            return Err(Error::invalid("an icon value is empty"));
        }
        let path = base.join(spec);
        let bytes = fs::read(&path)
            .map_err(|e| Error::invalid(format!("cannot read icon {}: {e}", path.display())))?;
        if ImageInfo::sniff(&bytes).is_none() {
            return Err(Error::invalid(format!(
                "icon {} is not a PNG, GIF or JPEG image",
                path.display()
            )));
        }
        Ok(Icon::File(path))
    }

    /// How the plan shows it: the path, or the `yoto:#` reference.
    pub fn describe(&self) -> String {
        match self {
            Icon::Yoto(id) => format!("yoto:#{id}"),
            Icon::File(p) => p.display().to_string(),
        }
    }
}

/// Icons for a playlist: per track number (1-based) plus a fallback.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IconMap {
    pub source: Option<PathBuf>,
    pub default: Option<Icon>,
    pub tracks: BTreeMap<usize, Icon>,
}

impl IconMap {
    /// Load an `icons.json`; file values resolve relative to its folder.
    pub fn load(path: &Path, track_count: usize) -> Result<IconMap> {
        let text = fs::read_to_string(path)
            .map_err(|e| Error::invalid(format!("cannot read {}: {e}", path.display())))?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| Error::invalid(format!("{} is not valid JSON: {e}", path.display())))?;
        let Value::Object(entries) = value else {
            return Err(Error::invalid(format!(
                "{} must be a JSON object",
                path.display()
            )));
        };
        let base = path.parent().unwrap_or(Path::new("."));
        let mut map = IconMap {
            source: Some(path.to_path_buf()),
            ..IconMap::default()
        };
        for (key, value) in entries {
            let spec = value.as_str().ok_or_else(|| {
                Error::invalid(format!(
                    "{}: the value for {key:?} must be a string",
                    path.display()
                ))
            })?;
            let icon = Icon::parse(spec, base)?;
            if key == "default" {
                map.default = Some(icon);
                continue;
            }
            let n: usize = key.parse().ok().filter(|n| (1..=track_count).contains(n)).ok_or_else(|| {
                Error::invalid(format!(
                    "{}: key {key:?} must be \"default\" or a track number from 1 to {track_count}",
                    path.display()
                ))
            })?;
            map.tracks.insert(n, icon);
        }
        Ok(map)
    }

    /// The icon for track `number` (1-based), if any was chosen.
    pub fn for_track(&self, number: usize) -> Option<&Icon> {
        self.tracks.get(&number).or(self.default.as_ref())
    }
}

/// The first `icons.json` among `dirs`.
pub fn find(dirs: &[&Path]) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join(ICONS_FILE))
        .find(|p| p.is_file())
}

/// What the upload needs to know about an icon image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub content_type: &'static str,
    /// Already a 16x16 icon Yoto accepts unconverted (RGBA PNG or GIF), so it
    /// can be stored exactly as drawn instead of resized and dimmed.
    pub ready: bool,
}

impl ImageInfo {
    pub fn sniff(bytes: &[u8]) -> Option<ImageInfo> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            // IHDR follows the signature: length, type, width, height, depth, colour type.
            let be32 = |at: usize| {
                bytes
                    .get(at..at + 4)
                    .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            };
            let ready = &bytes.get(12..16).unwrap_or_default() == b"IHDR"
                && be32(16) == Some(16)
                && be32(20) == Some(16)
                && bytes.get(24) == Some(&8)
                && bytes.get(25) == Some(&6);
            Some(ImageInfo {
                content_type: "image/png",
                ready,
            })
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            let le16 = |at: usize| {
                bytes
                    .get(at..at + 2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
            };
            let ready = le16(6) == Some(16) && le16(8) == Some(16);
            Some(ImageInfo {
                content_type: "image/gif",
                ready,
            })
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(ImageInfo {
                content_type: "image/jpeg",
                ready: false,
            })
        } else {
            None
        }
    }
}

/// Keep public icons whose title or tags contain every query word.
pub fn matches(icon: &Value, words: &[String]) -> bool {
    let mut hay = icon
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    for tag in icon
        .get("publicTags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(t) = tag.as_str() {
            hay.push(' ');
            hay.push_str(&t.to_lowercase());
        }
    }
    words.iter().all(|w| hay.contains(&w.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "aUm9i3ex3qqAMYBv-i-O-pYMKuMJGICtR3Vhf289u2Q";

    fn png(w: u32, h: u32, depth: u8, colour: u8) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[depth, colour, 0, 0, 0]);
        b
    }

    #[test]
    fn only_16x16_rgba_png_and_16x16_gif_skip_conversion() {
        assert_eq!(
            ImageInfo::sniff(&png(16, 16, 8, 6)),
            Some(ImageInfo {
                content_type: "image/png",
                ready: true
            })
        );
        assert!(
            !ImageInfo::sniff(&png(16, 16, 8, 3)).unwrap().ready,
            "palette PNG needs converting"
        );
        assert!(
            !ImageInfo::sniff(&png(16, 16, 8, 2)).unwrap().ready,
            "RGB without alpha needs converting"
        );
        assert!(!ImageInfo::sniff(&png(32, 32, 8, 6)).unwrap().ready);
        assert!(!ImageInfo::sniff(&png(16, 32, 8, 6)).unwrap().ready);
        assert!(
            ImageInfo::sniff(b"GIF89a\x10\x00\x10\x00rest")
                .unwrap()
                .ready
        );
        assert!(
            !ImageInfo::sniff(b"GIF89a\x20\x00\x10\x00rest")
                .unwrap()
                .ready
        );
        assert!(!ImageInfo::sniff(&[0xFF, 0xD8, 0xFF, 0xE0]).unwrap().ready);
        assert_eq!(ImageInfo::sniff(b"<svg"), None);
        assert!(
            !ImageInfo::sniff(b"\x89PNG\r\n\x1a\n").unwrap().ready,
            "truncated PNG"
        );
    }

    #[test]
    fn icon_specs_are_yoto_ids_or_existing_images() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.png"), png(16, 16, 8, 6)).unwrap();
        fs::write(tmp.path().join("notes.txt"), "x").unwrap();
        assert_eq!(
            Icon::parse(&format!("yoto:#{ID}"), tmp.path()).unwrap(),
            Icon::Yoto(ID.into())
        );
        assert_eq!(
            Icon::parse("a.png", tmp.path()).unwrap(),
            Icon::File(tmp.path().join("a.png"))
        );
        for bad in [
            "yoto:#short",
            &format!("yoto:#{}!", &ID[..42]),
            "missing.png",
            "notes.txt",
            " ",
        ] {
            assert_eq!(
                Icon::parse(bad, tmp.path()).unwrap_err().kind(),
                "invalid_input",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn map_assigns_by_number_with_default_and_rejects_bad_keys() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("icons")).unwrap();
        fs::write(tmp.path().join("icons/1.png"), png(16, 16, 8, 6)).unwrap();
        let path = tmp.path().join(ICONS_FILE);
        fs::write(
            &path,
            json!({"default": format!("yoto:#{ID}"), "1": "icons/1.png"}).to_string(),
        )
        .unwrap();
        let map = IconMap::load(&path, 3).unwrap();
        assert_eq!(
            map.for_track(1),
            Some(&Icon::File(tmp.path().join("icons/1.png")))
        );
        assert_eq!(map.for_track(3), Some(&Icon::Yoto(ID.into())));

        for bad in [
            json!({"4": format!("yoto:#{ID}")}),
            json!({"0": format!("yoto:#{ID}")}),
            json!({"one": format!("yoto:#{ID}")}),
            json!({"1": 5}),
            json!([1]),
        ] {
            fs::write(&path, bad.to_string()).unwrap();
            assert_eq!(
                IconMap::load(&path, 3).unwrap_err().kind(),
                "invalid_input",
                "{bad}"
            );
        }
    }

    #[test]
    fn no_default_leaves_unlisted_tracks_unset() {
        let map = IconMap {
            tracks: [(2, Icon::Yoto(ID.into()))].into(),
            ..IconMap::default()
        };
        assert_eq!(map.for_track(1), None);
        assert!(map.for_track(2).is_some());
    }

    #[test]
    fn search_needs_every_word_in_title_or_tags() {
        let icon =
            json!({"title": "Happy Seedling Plant", "publicTags": ["happy", "seedling", "garden"]});
        assert!(matches(&icon, &["plant".into(), "GARDEN".into()]));
        assert!(!matches(&icon, &["plant".into(), "dog".into()]));
        assert!(matches(&icon, &[]));
    }
}
