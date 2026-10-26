//! Upload a planned playlist: cover, icons, audio (skipping files Yoto
//! already has), bounded wait for transcoding, then one `POST /content`.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::api::{Client, Transcoded};
use crate::error::{Error, Result};
use crate::icons::{Icon, ImageInfo};
use crate::metadata::Metadata;
use crate::plan::{self, OnEnd, Plan};

/// The icon from Yoto's upload example, for tracks without a chosen icon.
pub const DEFAULT_ICON: &str = "yoto:#aUm9i3ex3qqAMYBv-i-O-pYMKuMJGICtR3Vhf289u2Q";

pub struct Options {
    pub transcode_timeout: Duration,
    pub upload_cover: bool,
    /// Replace this existing playlist instead of creating a new one.
    pub replace: Option<String>,
    pub metadata: Metadata,
}

/// One finished track, ready to go into the playlist.
#[derive(Debug, Clone)]
pub struct Uploaded {
    pub title: String,
    pub transcoded: Transcoded,
    /// Yoto already had this file, so it was not uploaded again.
    pub reused: bool,
    pub on_end: Option<OnEnd>,
}

/// One chapter of finished tracks.
pub struct ChapterBody<'a> {
    pub title: &'a str,
    /// `yoto:#<mediaId>` shown while the chapter's tracks play.
    pub icon: &'a str,
    pub tracks: &'a [Uploaded],
}

/// Run the upload. `log` receives one human progress line per step (stderr).
pub fn run(
    client: &Client,
    plan: &Plan,
    hashes: &[String],
    opts: &Options,
    mut log: impl FnMut(String),
) -> Result<Value> {
    let cover_url = match (&plan.cover, opts.upload_cover) {
        (Some(path), true) => {
            let bytes = std::fs::read(path).map_err(|e| {
                Error::invalid(format!("cannot read cover {}: {e}", path.display()))
            })?;
            let ct = plan::cover_content_type(&bytes).ok_or_else(|| {
                Error::invalid(format!("cover {} is neither JPEG nor PNG", path.display()))
            })?;
            log(format!("cover: uploading {}", path.display()));
            Some(client.upload_cover(bytes, ct)?)
        }
        _ => None,
    };

    let icon_refs = upload_icons(client, plan, &mut log)?;
    let done = upload_tracks(client, plan, hashes, opts.transcode_timeout, &mut log)?;

    let chapters: Vec<ChapterBody> = plan
        .chapters
        .iter()
        .zip(&icon_refs)
        .map(|(c, icon)| ChapterBody {
            title: &c.title,
            icon,
            tracks: &done[c.tracks.clone()],
        })
        .collect();
    let mut body = content_body(&plan.title, &chapters, cover_url.as_deref(), &opts.metadata)?;
    if let Some(bytes) = body
        .pointer("/metadata/media/fileSize")
        .and_then(Value::as_u64)
    {
        plan::check_transcoded_bytes(bytes)?;
    }
    match &opts.replace {
        Some(id) => {
            body["cardId"] = json!(id);
            log(format!("replacing playlist {id} with {:?}", plan.title));
        }
        None => log(format!("creating playlist {:?}", plan.title)),
    }
    let card = client.create_content(&body)?;
    let card_id = card
        .get("cardId")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Api {
            status: 200,
            message: "creating the playlist: response has no cardId".into(),
        })?;
    Ok(json!({
        "card_id": card_id,
        "title": plan.title,
        "chapter_count": chapters.len(),
        "track_count": done.len(),
        "uploaded": done.iter().filter(|u| !u.reused).count(),
        "reused": done.iter().filter(|u| u.reused).count(),
        "duration_seconds": body.pointer("/metadata/media/duration"),
        "bytes": body.pointer("/metadata/media/fileSize"),
        "cover": cover_url.is_some(),
        "custom_icons": icon_refs.iter().filter(|r| r.as_str() != DEFAULT_ICON).collect::<std::collections::BTreeSet<_>>().len(),
        "replaced": opts.replace.is_some(),
    }))
}

/// Upload (or find on Yoto) every track of `plan` and wait for each to be
/// transcoded, in order.
pub fn upload_tracks(
    client: &Client,
    plan: &Plan,
    hashes: &[String],
    transcode_timeout: Duration,
    log: &mut impl FnMut(String),
) -> Result<Vec<Uploaded>> {
    assert_eq!(hashes.len(), plan.tracks.len(), "one hash per track");
    let n = plan.tracks.len();
    let mut done = Vec::with_capacity(n);
    for (i, (track, sha)) in plan.tracks.iter().zip(hashes).enumerate() {
        let filename = track
            .path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let slot = client.upload_slot(sha, &filename)?;
        let reused = slot.upload_url.is_none();
        match &slot.upload_url {
            Some(url) => {
                log(format!(
                    "[{}/{n}] uploading {} ({:.1} MB)",
                    i + 1,
                    track.title,
                    track.bytes as f64 / 1e6
                ));
                client.put_audio(url, &track.path, track.format.content_type())?;
            }
            None => log(format!("[{}/{n}] already on Yoto: {}", i + 1, track.title)),
        }
        let transcoded = client.wait_transcoded(&slot.upload_id, transcode_timeout)?;
        if let Some(d) = transcoded.duration.filter(|d| *d > plan::MAX_TRACK_SECONDS) {
            return Err(Error::LimitExceeded {
                message: format!(
                    "{} lasts {d:.0} s after transcoding; a track may last at most {:.0} s",
                    track.title,
                    plan::MAX_TRACK_SECONDS
                ),
            });
        }
        done.push(Uploaded {
            title: track.title.clone(),
            transcoded,
            reused,
            on_end: track.on_end,
        });
    }
    Ok(done)
}

/// Resolve every chapter's icon to a `yoto:#` reference, uploading each
/// distinct image file once.
pub fn upload_icons(
    client: &Client,
    plan: &Plan,
    log: &mut impl FnMut(String),
) -> Result<Vec<String>> {
    let mut uploaded: BTreeMap<Icon, String> = BTreeMap::new();
    (1..=plan.chapters.len())
        .map(|number| match plan.icons.for_chapter(number) {
            None => Ok(DEFAULT_ICON.to_string()),
            Some(icon) => resolve_icon(client, icon, &mut uploaded, log),
        })
        .collect()
}

/// The `yoto:#` reference for `icon`, uploading an image file unless
/// `uploaded` already holds its reference.
pub fn resolve_icon(
    client: &Client,
    icon: &Icon,
    uploaded: &mut BTreeMap<Icon, String>,
    log: &mut impl FnMut(String),
) -> Result<String> {
    let path = match icon {
        Icon::Yoto(id) => return Ok(format!("yoto:#{id}")),
        Icon::File(path) => path,
    };
    if let Some(r) = uploaded.get(icon) {
        return Ok(r.clone());
    }
    let bytes = std::fs::read(path)
        .map_err(|e| Error::invalid(format!("cannot read icon {}: {e}", path.display())))?;
    let info = ImageInfo::sniff(&bytes).ok_or_else(|| {
        Error::invalid(format!(
            "icon {} is not a PNG, GIF or JPEG image",
            path.display()
        ))
    })?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "icon".into());
    log(format!(
        "icon: uploading {}{}",
        path.display(),
        if info.ready {
            ""
        } else {
            " (Yoto converts it to 16x16)"
        }
    ));
    let r = format!(
        "yoto:#{}",
        client.upload_icon(&bytes, &name, info.content_type, !info.ready)?
    );
    uploaded.insert(icon.clone(), r.clone());
    Ok(r)
}

/// The `POST /content` body. Chapters are numbered from 1, so the player's
/// skip buttons move between the book's parts; a chapter's tracks play in
/// order.
pub fn content_body(
    title: &str,
    chapters: &[ChapterBody],
    cover_url: Option<&str>,
    meta: &Metadata,
) -> Result<Value> {
    let mut out = Vec::with_capacity(chapters.len());
    let mut total_duration = 0.0;
    let mut total_size = 0u64;
    for (i, c) in chapters.iter().enumerate() {
        let label = (i + 1).to_string();
        let mut tracks = Vec::with_capacity(c.tracks.len());
        for (j, u) in c.tracks.iter().enumerate() {
            let t = &u.transcoded;
            // Duration and size are required track fields; Yoto must report them.
            let missing = |field: &str| Error::Api {
                status: 200,
                message: format!("transcode of {:?} reported no {field}", u.title),
            };
            let duration = t.duration.ok_or_else(|| missing("duration"))?;
            let file_size = t.file_size.ok_or_else(|| missing("fileSize"))?;
            let format = t.format.clone().ok_or_else(|| missing("format"))?;
            total_duration += duration;
            total_size += file_size;
            let mut track = json!({
                "key": format!("{:02}", j + 1),
                "title": u.title,
                "trackUrl": format!("yoto:#{}", t.sha256),
                "duration": duration,
                "fileSize": file_size,
                "format": format,
                "type": "audio",
                "overlayLabel": label,
                "display": {"icon16x16": c.icon},
            });
            if let Some(ch) = &t.channels {
                track["channels"] = ch.clone();
            }
            if let Some(end) = u.on_end {
                track["events"] = json!({"onEnd": {"cmd": end.name()}});
            }
            tracks.push(track);
        }
        out.push(json!({
            "key": format!("{:02}", i + 1),
            "title": c.title,
            "overlayLabel": label,
            "tracks": tracks,
            "display": {"icon16x16": c.icon},
        }));
    }
    let mut metadata = Map::new();
    metadata.insert(
        "media".into(),
        json!({
            "duration": total_duration,
            "fileSize": total_size,
            "readableFileSize": (total_size as f64 / 1024.0 / 1024.0 * 10.0).round() / 10.0,
        }),
    );
    if let Some(url) = cover_url {
        metadata.insert("cover".into(), json!({"imageL": url}));
    }
    meta.apply(&mut metadata)?;
    Ok(json!({
        "title": title,
        "content": {"chapters": out},
        "metadata": metadata,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uploaded(title: &str, sha: &str, duration: Option<f64>) -> Uploaded {
        Uploaded {
            title: title.into(),
            transcoded: Transcoded {
                sha256: sha.into(),
                duration,
                file_size: Some(1_048_576),
                channels: Some(json!("stereo")),
                format: Some("mp3".into()),
            },
            reused: false,
            on_end: None,
        }
    }

    fn single<'a>(tracks: &'a [Uploaded]) -> Vec<ChapterBody<'a>> {
        tracks
            .iter()
            .map(|t| ChapterBody {
                title: &t.title,
                icon: DEFAULT_ICON,
                tracks: std::slice::from_ref(t),
            })
            .collect()
    }

    #[test]
    fn body_has_one_numbered_chapter_per_track() {
        let tracks = [
            uploaded("Een", "s1", Some(10.0)),
            uploaded("Twee", "s2", Some(20.5)),
        ];
        let body = content_body(
            "Boer Boris",
            &single(&tracks),
            Some("https://cover.example/c.jpg"),
            &Metadata::default(),
        )
        .unwrap();
        let chapters = body["content"]["chapters"].as_array().unwrap();
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[1]["key"], "02");
        assert_eq!(chapters[1]["overlayLabel"], "2");
        assert_eq!(chapters[1]["title"], "Twee");
        let t = &chapters[1]["tracks"][0];
        assert_eq!(t["key"], "01");
        assert_eq!(t["trackUrl"], "yoto:#s2");
        assert_eq!(t["duration"], 20.5);
        assert_eq!(t["type"], "audio");
        assert_eq!(t["channels"], "stereo");
        assert_eq!(t["display"]["icon16x16"], DEFAULT_ICON);
        assert!(t.get("events").is_none());
        assert_eq!(chapters[1]["display"]["icon16x16"], DEFAULT_ICON);
        assert_eq!(body["metadata"]["media"]["duration"], 30.5);
        assert_eq!(body["metadata"]["media"]["fileSize"], 2_097_152);
        assert_eq!(body["metadata"]["media"]["readableFileSize"], 2.0);
        assert_eq!(
            body["metadata"]["cover"]["imageL"],
            "https://cover.example/c.jpg"
        );
        assert!(body["metadata"].get("author").is_none());
    }

    #[test]
    fn a_chapter_holds_its_tracks_in_order_with_their_end_actions() {
        let mut tracks = [
            uploaded("Intro", "s1", Some(1.0)),
            uploaded("Couplet", "s2", Some(2.0)),
            uploaded("Refrein", "s3", Some(3.0)),
        ];
        tracks[2].on_end = Some(OnEnd::Repeat);
        let icon = "yoto:#7Wt--qhx9t7PCSC8WBmGKJvlG53Z1uk7bN1dE9wGWJ0";
        let chapters = [
            ChapterBody {
                title: "Intro",
                icon: DEFAULT_ICON,
                tracks: &tracks[..1],
            },
            ChapterBody {
                title: "Lied",
                icon,
                tracks: &tracks[1..],
            },
        ];
        let body = content_body("x", &chapters, None, &Metadata::default()).unwrap();
        let lied = &body["content"]["chapters"][1];
        assert_eq!(lied["key"], "02");
        assert_eq!(lied["title"], "Lied");
        let keys: Vec<_> = lied["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["key"].as_str().unwrap(), t["title"].as_str().unwrap()))
            .collect();
        assert_eq!(keys, [("01", "Couplet"), ("02", "Refrein")]);
        assert_eq!(lied["tracks"][1]["overlayLabel"], "2");
        assert_eq!(lied["tracks"][1]["display"]["icon16x16"], icon);
        assert_eq!(lied["tracks"][1]["events"]["onEnd"]["cmd"], "repeat");
        assert!(lied["tracks"][0].get("events").is_none());
        assert_eq!(body["metadata"]["media"]["duration"], 6.0);
    }

    #[test]
    fn metadata_goes_into_the_body() {
        let tracks = [uploaded("Een", "s1", Some(1.0))];
        let meta = Metadata {
            authors: Some(vec!["Ted van Lieshout".into()]),
            description: Some("Over een boer".into()),
            ..Metadata::default()
        };
        let body = content_body("x", &single(&tracks), None, &meta).unwrap();
        assert_eq!(body["metadata"]["author"], "Ted van Lieshout");
        assert_eq!(body["metadata"]["description"], "Over een boer");
        assert_eq!(body["metadata"]["media"]["duration"], 1.0);
    }

    #[test]
    fn missing_transcode_duration_is_an_error_not_zero() {
        let tracks = [uploaded("Een", "s1", None)];
        let err = content_body("x", &single(&tracks), None, &Metadata::default()).unwrap_err();
        assert_eq!(err.kind(), "api_error");
        assert!(err.to_string().contains("duration"));
    }

    #[test]
    fn no_cover_means_no_cover_field() {
        let tracks = [uploaded("Een", "s1", Some(1.0))];
        let body = content_body("x", &single(&tracks), None, &Metadata::default()).unwrap();
        assert!(body["metadata"].get("cover").is_none());
    }
}
