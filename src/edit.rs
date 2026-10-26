//! Changes to an existing playlist, made on the card Yoto returns and sent
//! back whole with its `cardId`, so cards linked to it keep working.
//!
//! Chapter keys never change: the player remembers where it was by key, so
//! a moved chapter keeps its place in the listener's progress. The number
//! shown on the player (`overlayLabel`) follows the position instead.

use serde_json::{Map, Value, json};

use crate::error::{Error, Result};
use crate::metadata::Metadata;
use crate::plan::MAX_TITLE_CHARS;

/// The card fields a `POST /content` carries; everything else Yoto keeps
/// itself (owner, timestamps of the last change).
const KEPT_FIELDS: &[&str] = &[
    "cardId",
    "title",
    "content",
    "metadata",
    "createdAt",
    "slug",
];

/// A copy of `card` holding only what is sent back.
pub fn editable(card: &Value) -> Result<Value> {
    let Value::Object(fields) = card else {
        return Err(Error::Api {
            status: 200,
            message: "the playlist is not a JSON object".into(),
        });
    };
    let kept: Map<String, Value> = fields
        .iter()
        .filter(|(k, _)| KEPT_FIELDS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let card = Value::Object(kept);
    let Some(chapters) = card.pointer("/content/chapters").and_then(Value::as_array) else {
        return Err(Error::Api {
            status: 200,
            message: "the playlist has no chapters list".into(),
        });
    };
    // Sent back, a track must name its audio as Yoto stores it. A signed
    // download link in its place would stop playing once the link expires.
    let tracks = chapters.iter().flat_map(|c| {
        c.get("tracks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
    });
    for t in tracks {
        let url = t
            .get("trackUrl")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !url.starts_with("yoto:#") {
            return Err(Error::Api {
                status: 200,
                message: format!(
                    "track {:?} does not point at audio stored on Yoto, so the playlist cannot be edited safely",
                    t.get("title").and_then(Value::as_str).unwrap_or("?")
                ),
            });
        }
    }
    Ok(card)
}

fn chapters(card: &mut Value) -> &mut Vec<Value> {
    card.pointer_mut("/content/chapters")
        .and_then(Value::as_array_mut)
        .expect("editable() checked the chapters list")
}

pub fn chapter_count(card: &Value) -> usize {
    card.pointer("/content/chapters")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// The 0-based index of chapter `number` (1-based), or an error naming the range.
fn index(card: &Value, number: usize) -> Result<usize> {
    let n = chapter_count(card);
    if number == 0 || number > n {
        return Err(Error::invalid(format!(
            "chapter {number} does not exist; the playlist has chapters 1 to {n}"
        )));
    }
    Ok(number - 1)
}

fn check_title(title: &str, what: &str) -> Result<String> {
    let title = title.trim();
    let chars = title.chars().count();
    if chars == 0 || chars > MAX_TITLE_CHARS {
        return Err(Error::invalid(format!(
            "the {what} must be 1 to {MAX_TITLE_CHARS} characters (it is {chars})"
        )));
    }
    Ok(title.to_string())
}

pub fn set_title(card: &mut Value, title: &str) -> Result<()> {
    card["title"] = json!(check_title(title, "playlist title")?);
    Ok(())
}

pub fn apply_metadata(card: &mut Value, meta: &Metadata) -> Result<()> {
    meta.validate()?;
    if !card.get("metadata").is_some_and(Value::is_object) {
        card["metadata"] = json!({});
    }
    let map = card["metadata"]
        .as_object_mut()
        .expect("just made an object");
    meta.apply(map)
}

/// Rename chapter `number`. A chapter of one track is that track, so the
/// track is renamed with it.
pub fn rename_chapter(card: &mut Value, number: usize, title: &str) -> Result<()> {
    let i = index(card, number)?;
    let title = check_title(title, "chapter title")?;
    let ch = &mut chapters(card)[i];
    ch["title"] = json!(title);
    if let Some([track]) = ch
        .get_mut("tracks")
        .and_then(Value::as_array_mut)
        .map(Vec::as_mut_slice)
    {
        track["title"] = json!(title);
    }
    Ok(())
}

/// Remove the given chapters (1-based numbers, any order). At least one
/// chapter must remain.
pub fn delete_chapters(card: &mut Value, numbers: &[usize]) -> Result<()> {
    let mut idx = numbers
        .iter()
        .map(|n| index(card, *n))
        .collect::<Result<Vec<_>>>()?;
    idx.sort_unstable();
    idx.dedup();
    if idx.len() >= chapter_count(card) {
        return Err(Error::invalid(
            "a playlist needs at least one chapter; delete the playlist instead",
        ));
    }
    let old = label_snapshot(card);
    let kept: Vec<_> = (0..old.len()).filter(|i| !idx.contains(i)).collect();
    let shuffle = shuffle_after(card, &kept)?;
    let list = chapters(card);
    for i in idx.iter().rev() {
        list.remove(*i);
    }
    relabel(card, &kept, &old);
    set_shuffle(card, shuffle);
    Ok(())
}

/// Move chapter `from` so it becomes chapter `to` (both 1-based).
pub fn move_chapter(card: &mut Value, from: usize, to: usize) -> Result<()> {
    let f = index(card, from)?;
    let t = index(card, to)?;
    let old = label_snapshot(card);
    let mut order: Vec<usize> = (0..old.len()).collect();
    let moved = order.remove(f);
    order.insert(t, moved);
    let shuffle = shuffle_after(card, &order)?;
    let list = chapters(card);
    let ch = list.remove(f);
    list.insert(t, ch);
    relabel(card, &order, &old);
    set_shuffle(card, shuffle);
    Ok(())
}

/// The card's shuffle groups after the chapters are reordered so that
/// position `p` holds old chapter `order[p]`. Groups name chapters by
/// 0-based position, so each one follows its own chapters: it shifts,
/// shrinks (its limit no larger than what is left), or goes once empty.
/// `None` when the card has no shuffle groups.
fn shuffle_after(card: &Value, order: &[usize]) -> Result<Option<Value>> {
    let Some(groups) = card.pointer("/content/config/shuffle") else {
        return Ok(None);
    };
    let unreadable = || {
        Error::invalid(
            "the playlist's shuffle settings cannot be read, so its chapters cannot be reordered safely",
        )
    };
    let groups = groups.as_array().ok_or_else(unreadable)?;
    let mut out = Vec::new();
    for g in groups {
        let field = |name: &str| g.get(name).and_then(Value::as_u64).ok_or_else(unreadable);
        let (start, end, limit) = (field("start")?, field("end")?, field("limit")?);
        let at: Vec<u64> = order
            .iter()
            .enumerate()
            .filter(|(_, old)| (start..=end).contains(&(**old as u64)))
            .map(|(p, _)| p as u64)
            .collect();
        let (Some(&first), Some(&last)) = (at.first(), at.last()) else {
            continue;
        };
        let count = at.len() as u64;
        if last - first + 1 != count {
            return Err(Error::invalid(format!(
                "this would put a chapter that plays in order inside the shuffled chapters {} to {}; change the shuffle in the Yoto app first",
                start + 1,
                end + 1
            )));
        }
        let mut g = g.clone();
        g["start"] = json!(first);
        g["end"] = json!(last);
        g["limit"] = json!(limit.min(count));
        out.push(g);
    }
    Ok(Some(Value::Array(out)))
}

fn set_shuffle(card: &mut Value, shuffle: Option<Value>) {
    if let Some(s) = shuffle {
        card["content"]["config"]["shuffle"] = s;
    }
}

/// Show `icon` (a `yoto:#` reference) while chapter `number` plays.
pub fn set_chapter_icon(card: &mut Value, number: usize, icon: &str) -> Result<()> {
    let i = index(card, number)?;
    let ch = &mut chapters(card)[i];
    set_display_icon(ch, icon);
    if let Some(tracks) = ch.get_mut("tracks").and_then(Value::as_array_mut) {
        for t in tracks {
            set_display_icon(t, icon);
        }
    }
    Ok(())
}

fn set_display_icon(v: &mut Value, icon: &str) {
    if !v.get("display").is_some_and(Value::is_object) {
        v["display"] = json!({});
    }
    v["display"]["icon16x16"] = json!(icon);
    // A URL icon would be shown instead of the reference; drop it.
    if let Some(d) = v["display"].as_object_mut() {
        d.remove("iconUrl16x16");
    }
}

/// Add `new` chapters at the end. Each gets the next free two-digit key, and
/// its number labels continue from the last chapter.
pub fn append_chapters(card: &mut Value, new: Vec<Value>) -> Result<()> {
    let existing = chapter_count(card);
    let mut used: Vec<String> = chapters(card)
        .iter()
        .filter_map(|c| c.get("key").and_then(Value::as_str).map(str::to_string))
        .collect();
    let mut next = used
        .iter()
        .filter_map(|k| k.parse::<usize>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    for (j, mut ch) in new.into_iter().enumerate() {
        let mut key = format!("{next:02}");
        while used.contains(&key) {
            next += 1;
            key = format!("{next:02}");
        }
        next += 1;
        used.push(key.clone());
        let old_label = ch.get("overlayLabel").cloned();
        let label = (existing + j + 1).to_string();
        ch["key"] = json!(key);
        ch["overlayLabel"] = json!(label);
        if let Some(tracks) = ch.get_mut("tracks").and_then(Value::as_array_mut) {
            for t in tracks {
                if t.get("overlayLabel") == old_label.as_ref() {
                    t["overlayLabel"] = json!(label);
                }
            }
        }
        chapters(card).push(ch);
    }
    Ok(())
}

pub fn track_count(card: &Value) -> usize {
    card.pointer("/content/chapters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("tracks").and_then(Value::as_array))
        .map(Vec::len)
        .sum()
}

/// The sum of the track sizes the card reports. A track without a size adds
/// nothing, so this is a lower bound: exceeding a limit with it is certain.
pub fn known_bytes(card: &Value) -> u64 {
    card.pointer("/content/chapters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("tracks").and_then(Value::as_array))
        .flatten()
        .filter_map(|t| t.get("fileSize").and_then(Value::as_u64))
        .sum()
}

/// Set the totals in `metadata.media` to the sum of the tracks, keeping its
/// other fields. When a track does not report its duration or size the
/// totals are removed instead: a missing length is not a length of zero.
pub fn recompute_media(card: &mut Value) {
    let mut totals = Some((0.0, 0u64));
    let tracks = card
        .pointer("/content/chapters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("tracks").and_then(Value::as_array))
        .flatten();
    for t in tracks {
        match (
            t.get("duration").and_then(Value::as_f64),
            t.get("fileSize").and_then(Value::as_u64),
        ) {
            (Some(d), Some(s)) => totals = totals.map(|(td, ts)| (td + d, ts + s)),
            _ => totals = None,
        }
    }
    if !card.get("metadata").is_some_and(Value::is_object) {
        card["metadata"] = json!({});
    }
    if !card["metadata"].get("media").is_some_and(Value::is_object) {
        card["metadata"]["media"] = json!({});
    }
    let media = card["metadata"]["media"]
        .as_object_mut()
        .expect("made an object above");
    match totals {
        Some((duration, size)) => {
            media.insert("duration".into(), json!(duration));
            media.insert("fileSize".into(), json!(size));
            media.insert(
                "readableFileSize".into(),
                json!((size as f64 / 1024.0 / 1024.0 * 10.0).round() / 10.0),
            );
        }
        // A total that cannot be counted is removed, not left at a value
        // that no longer matches the tracks.
        None => {
            for field in ["duration", "fileSize", "readableFileSize"] {
                media.remove(field);
            }
        }
    }
}

/// Each chapter's `overlayLabel`, before a reorder.
fn label_snapshot(card: &Value) -> Vec<Option<Value>> {
    card.pointer("/content/chapters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|c| c.get("overlayLabel").cloned())
        .collect()
}

/// After a reorder, `order[i]` is the old index of the chapter now at `i`.
/// A chapter labelled with its old position number is labelled with its new
/// one, and so are its tracks that carried the same label; any other label
/// (a letter, a custom word) was chosen by someone and stays.
fn relabel(card: &mut Value, order: &[usize], old: &[Option<Value>]) {
    for (i, ch) in chapters(card).iter_mut().enumerate() {
        let before = &old[order[i]];
        let was_position = before
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|l| l == (order[i] + 1).to_string());
        if !was_position {
            continue;
        }
        let label = json!((i + 1).to_string());
        ch["overlayLabel"] = label.clone();
        if let Some(tracks) = ch.get_mut("tracks").and_then(Value::as_array_mut) {
            for t in tracks {
                if t.get("overlayLabel") == before.as_ref() {
                    t["overlayLabel"] = label.clone();
                }
            }
        }
    }
}

/// The chapters as `content edit` and friends report them.
pub fn chapter_summary(card: &Value) -> Vec<Value> {
    card.pointer("/content/chapters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, c)| {
            json!({
                "number": i + 1,
                "key": c.get("key"),
                "title": c.get("title"),
                "track_count": c.get("tracks").and_then(Value::as_array).map_or(0, Vec::len),
                "icon": c.pointer("/display/icon16x16"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICON: &str = "yoto:#7Wt--qhx9t7PCSC8WBmGKJvlG53Z1uk7bN1dE9wGWJ0";

    fn chapter(key: &str, label: &str, title: &str, tracks: usize) -> Value {
        let tracks: Vec<Value> = (1..=tracks)
            .map(|t| {
                json!({"key": format!("{t:02}"), "title": if tracks == 1 { title.to_string() } else { format!("{title} {t}") },
                       "trackUrl": format!("yoto:#{key}{t}"),
                       "overlayLabel": label, "duration": 10.0, "fileSize": 100,
                       "display": {"icon16x16": "yoto:#old"}})
            })
            .collect();
        json!({"key": key, "title": title, "overlayLabel": label, "tracks": tracks,
               "display": {"icon16x16": "yoto:#old"}})
    }

    fn card() -> Value {
        json!({
            "cardId": "abcde",
            "title": "Boer Boris",
            "createdAt": "2026-10-01T10:00:00.000Z",
            "updatedAt": "2026-10-02T10:00:00.000Z",
            "userId": "auth0|someone",
            "content": {"chapters": [
                chapter("01", "1", "Een", 1),
                chapter("02", "2", "Twee", 2),
                chapter("03", "A", "Drie", 1),
            ], "config": {"autoadvance": "next"}},
            "metadata": {"author": "Ted", "media": {"duration": 1.0}},
        })
    }

    /// A six-chapter card whose chapters 2 to 5 (indexes 1 to 4) shuffle,
    /// playing three of them.
    fn shuffled() -> Value {
        let mut c = card();
        let list = c["content"]["chapters"].as_array_mut().unwrap();
        list.clear();
        for (i, t) in ["A", "B", "C", "D", "E", "F"].iter().enumerate() {
            list.push(chapter(
                &format!("{:02}", i + 1),
                &(i + 1).to_string(),
                t,
                1,
            ));
        }
        c["content"]["config"]["shuffle"] = json!([{"start": 1, "end": 4, "limit": 3}]);
        c
    }

    fn shuffle(card: &Value) -> Value {
        card["content"]["config"]["shuffle"].clone()
    }

    #[test]
    fn deleting_chapters_keeps_shuffle_groups_on_the_same_chapters() {
        // A chapter before the group shifts it.
        let mut c = shuffled();
        delete_chapters(&mut c, &[1]).unwrap();
        assert_eq!(shuffle(&c), json!([{"start": 0, "end": 3, "limit": 3}]));
        // Chapters inside it shrink it, and the limit with it.
        let mut c = shuffled();
        delete_chapters(&mut c, &[3, 4]).unwrap();
        assert_eq!(shuffle(&c), json!([{"start": 1, "end": 2, "limit": 2}]));
        // A chapter after it changes nothing.
        let mut c = shuffled();
        delete_chapters(&mut c, &[6]).unwrap();
        assert_eq!(shuffle(&c), json!([{"start": 1, "end": 4, "limit": 3}]));
        // A group whose chapters are all gone goes too.
        let mut c = shuffled();
        delete_chapters(&mut c, &[2, 3, 4, 5]).unwrap();
        assert_eq!(shuffle(&c), json!([]));
    }

    #[test]
    fn moving_chapters_keeps_shuffle_groups_on_the_same_chapters() {
        // A fixed chapter moved past the group: the group shifts.
        let mut c = shuffled();
        move_chapter(&mut c, 1, 6).unwrap();
        assert_eq!(titles(&c), ["B", "C", "D", "E", "F", "A"]);
        assert_eq!(shuffle(&c), json!([{"start": 0, "end": 3, "limit": 3}]));
        // Within the group: unchanged.
        let mut c = shuffled();
        move_chapter(&mut c, 2, 5).unwrap();
        assert_eq!(shuffle(&c), json!([{"start": 1, "end": 4, "limit": 3}]));
        // A fixed chapter dropped into the middle would start shuffling.
        let mut c = shuffled();
        let err = move_chapter(&mut c, 6, 3).unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
        assert!(err.to_string().contains("shuffle"), "{err}");
        assert_eq!(titles(&c), ["A", "B", "C", "D", "E", "F"], "card untouched");
        // So would a shuffled chapter moved away from the others.
        let mut c = shuffled();
        assert_eq!(
            move_chapter(&mut c, 3, 1).unwrap_err().kind(),
            "invalid_input"
        );
    }

    #[test]
    fn an_unreadable_shuffle_group_stops_a_reorder() {
        let mut c = shuffled();
        c["content"]["config"]["shuffle"] = json!([{"start": "1", "end": 4, "limit": 3}]);
        assert_eq!(
            delete_chapters(&mut c, &[1]).unwrap_err().kind(),
            "invalid_input"
        );
        assert_eq!(titles(&c).len(), 6);
    }

    fn titles(card: &Value) -> Vec<String> {
        chapter_summary(card)
            .iter()
            .map(|c| c["title"].as_str().unwrap().to_string())
            .collect()
    }

    fn labels(card: &Value) -> Vec<(String, String)> {
        card["content"]["chapters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["key"].as_str().unwrap().into(),
                    c["overlayLabel"].as_str().unwrap().into(),
                )
            })
            .collect()
    }

    #[test]
    fn editable_keeps_what_is_sent_back_and_drops_server_fields() {
        let e = editable(&card()).unwrap();
        assert_eq!(e["cardId"], "abcde");
        assert_eq!(e["createdAt"], "2026-10-01T10:00:00.000Z");
        assert_eq!(e["content"]["config"]["autoadvance"], "next");
        assert_eq!(e["metadata"]["author"], "Ted");
        assert!(e.get("updatedAt").is_none());
        assert!(e.get("userId").is_none());
        assert_eq!(
            editable(&json!({"cardId": "x", "content": {}}))
                .unwrap_err()
                .kind(),
            "api_error"
        );
        let mut signed = card();
        signed["content"]["chapters"][1]["tracks"][1]["trackUrl"] =
            json!("https://secure.example/audio.mp3?Signature=x");
        assert_eq!(editable(&signed).unwrap_err().kind(), "api_error");
    }

    #[test]
    fn rename_renames_a_single_track_with_its_chapter() {
        let mut c = editable(&card()).unwrap();
        rename_chapter(&mut c, 1, " Een nieuw ").unwrap();
        assert_eq!(c["content"]["chapters"][0]["title"], "Een nieuw");
        assert_eq!(
            c["content"]["chapters"][0]["tracks"][0]["title"],
            "Een nieuw"
        );
        rename_chapter(&mut c, 2, "Lied").unwrap();
        assert_eq!(c["content"]["chapters"][1]["title"], "Lied");
        assert_eq!(c["content"]["chapters"][1]["tracks"][0]["title"], "Twee 1");
        assert_eq!(
            rename_chapter(&mut c, 4, "x").unwrap_err().kind(),
            "invalid_input"
        );
        assert_eq!(
            rename_chapter(&mut c, 0, "x").unwrap_err().kind(),
            "invalid_input"
        );
        assert_eq!(
            rename_chapter(&mut c, 1, "  ").unwrap_err().kind(),
            "invalid_input"
        );
    }

    #[test]
    fn move_keeps_keys_and_renumbers_position_labels_only() {
        let mut c = editable(&card()).unwrap();
        move_chapter(&mut c, 1, 3).unwrap();
        assert_eq!(titles(&c), ["Twee", "Drie", "Een"]);
        assert_eq!(
            labels(&c),
            [
                ("02".into(), "1".into()),
                ("03".into(), "A".into()),
                ("01".into(), "3".into())
            ]
        );
        // The tracks of a renumbered chapter follow it; others keep theirs.
        assert_eq!(
            c["content"]["chapters"][0]["tracks"][1]["overlayLabel"],
            "1"
        );
        assert_eq!(
            c["content"]["chapters"][2]["tracks"][0]["overlayLabel"],
            "3"
        );
        move_chapter(&mut c, 3, 1).unwrap();
        assert_eq!(titles(&c), ["Een", "Twee", "Drie"]);
        assert_eq!(labels(&c)[0], ("01".into(), "1".into()));
        assert_eq!(
            move_chapter(&mut c, 1, 4).unwrap_err().kind(),
            "invalid_input"
        );
    }

    #[test]
    fn delete_removes_chapters_and_closes_the_numbering() {
        let mut c = editable(&card()).unwrap();
        delete_chapters(&mut c, &[1, 1]).unwrap();
        assert_eq!(titles(&c), ["Twee", "Drie"]);
        assert_eq!(
            labels(&c),
            [("02".into(), "1".into()), ("03".into(), "A".into())]
        );
        assert_eq!(
            delete_chapters(&mut c, &[1, 2]).unwrap_err().kind(),
            "invalid_input"
        );
        assert_eq!(
            delete_chapters(&mut c, &[3]).unwrap_err().kind(),
            "invalid_input"
        );
        assert_eq!(
            titles(&c),
            ["Twee", "Drie"],
            "a failed delete changes nothing"
        );
    }

    #[test]
    fn icon_is_set_on_the_chapter_and_every_track() {
        let mut c = editable(&card()).unwrap();
        c["content"]["chapters"][1]["display"]["iconUrl16x16"] = json!("https://x/i.png");
        set_chapter_icon(&mut c, 2, ICON).unwrap();
        let ch = &c["content"]["chapters"][1];
        assert_eq!(ch["display"], json!({"icon16x16": ICON}));
        assert_eq!(ch["tracks"][0]["display"]["icon16x16"], ICON);
        assert_eq!(ch["tracks"][1]["display"]["icon16x16"], ICON);
        assert_eq!(
            c["content"]["chapters"][0]["display"]["icon16x16"],
            "yoto:#old"
        );
    }

    #[test]
    fn appended_chapters_get_free_keys_and_following_labels() {
        let mut c = editable(&card()).unwrap();
        // A key of "04" already in use must be skipped, not duplicated.
        c["content"]["chapters"][2]["key"] = json!("04");
        append_chapters(
            &mut c,
            vec![chapter("01", "1", "Vier", 1), chapter("02", "2", "Vijf", 2)],
        )
        .unwrap();
        assert_eq!(titles(&c), ["Een", "Twee", "Drie", "Vier", "Vijf"]);
        let l = labels(&c);
        assert_eq!(l[3], ("05".into(), "4".into()));
        assert_eq!(l[4], ("06".into(), "5".into()));
        assert_eq!(
            c["content"]["chapters"][4]["tracks"][1]["overlayLabel"],
            "5"
        );
    }

    #[test]
    fn media_totals_follow_the_tracks_and_are_dropped_when_one_is_unknown() {
        let mut c = editable(&card()).unwrap();
        recompute_media(&mut c);
        assert_eq!(c["metadata"]["media"]["duration"], 40.0);
        assert_eq!(c["metadata"]["media"]["fileSize"], 400);
        assert_eq!(c["metadata"]["author"], "Ted");
        let mut c = editable(&card()).unwrap();
        c["content"]["chapters"][0]["tracks"][0]
            .as_object_mut()
            .unwrap()
            .remove("duration");
        c["metadata"]["media"]["hasStreams"] = json!(false);
        recompute_media(&mut c);
        assert!(c["metadata"]["media"].get("duration").is_none());
        assert!(c["metadata"]["media"].get("fileSize").is_none());
        assert_eq!(c["metadata"]["media"]["hasStreams"], false);
    }

    #[test]
    fn title_and_metadata_edits() {
        let mut c = editable(&card()).unwrap();
        set_title(&mut c, " Nieuw ").unwrap();
        assert_eq!(c["title"], "Nieuw");
        assert_eq!(set_title(&mut c, "").unwrap_err().kind(), "invalid_input");
        apply_metadata(
            &mut c,
            &Metadata {
                authors: Some(vec![String::new()]),
                description: Some("Over een boer".into()),
                ..Metadata::default()
            },
        )
        .unwrap();
        assert!(c["metadata"].get("author").is_none());
        assert_eq!(c["metadata"]["description"], "Over een boer");
        assert_eq!(c["metadata"]["media"]["duration"], 1.0);
        let mut bare = json!({"cardId": "x", "content": {"chapters": []}});
        apply_metadata(
            &mut bare,
            &Metadata {
                min_age: Some(3),
                ..Metadata::default()
            },
        )
        .unwrap();
        assert_eq!(bare["metadata"]["minAge"], 3);
    }
}
