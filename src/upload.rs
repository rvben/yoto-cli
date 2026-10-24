//! Upload a planned playlist: cover, icons, audio (skipping files Yoto
//! already has), bounded wait for transcoding, then one `POST /content`.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};

use crate::api::{Client, Transcoded};
use crate::error::{Error, Result};
use crate::icons::{Icon, ImageInfo};
use crate::plan::{self, Plan};

/// The icon from Yoto's upload example, for tracks without a chosen icon.
pub const DEFAULT_ICON: &str = "yoto:#aUm9i3ex3qqAMYBv-i-O-pYMKuMJGICtR3Vhf289u2Q";

pub struct Options {
    pub transcode_timeout: Duration,
    pub upload_cover: bool,
    /// Replace this existing playlist instead of creating a new one.
    pub replace: Option<String>,
}

/// One finished track, ready to go into the playlist.
#[derive(Debug, Clone)]
pub struct Uploaded {
    pub title: String,
    pub transcoded: Transcoded,
    /// `yoto:#<mediaId>` shown while this track plays.
    pub icon: String,
    /// Yoto already had this file, so it was not uploaded again.
    pub reused: bool,
}

/// Run the upload. `log` receives one human progress line per step (stderr).
pub fn run(
    client: &Client,
    plan: &Plan,
    hashes: &[String],
    opts: &Options,
    mut log: impl FnMut(String),
) -> Result<Value> {
    assert_eq!(hashes.len(), plan.tracks.len(), "one hash per track");

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
        let transcoded = client.wait_transcoded(&slot.upload_id, opts.transcode_timeout)?;
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
            icon: icon_refs[i].clone(),
        });
    }

    let mut body = content_body(&plan.title, &done, cover_url.as_deref())?;
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

/// Resolve every track's icon to a `yoto:#` reference, uploading each
/// distinct image file once.
fn upload_icons(client: &Client, plan: &Plan, log: &mut impl FnMut(String)) -> Result<Vec<String>> {
    let mut uploaded: BTreeMap<&Icon, String> = BTreeMap::new();
    let mut refs = Vec::with_capacity(plan.tracks.len());
    for number in 1..=plan.tracks.len() {
        let reference = match plan.icons.for_track(number) {
            None => DEFAULT_ICON.to_string(),
            Some(Icon::Yoto(id)) => format!("yoto:#{id}"),
            Some(icon @ Icon::File(path)) => match uploaded.get(icon) {
                Some(r) => r.clone(),
                None => {
                    let bytes = std::fs::read(path).map_err(|e| {
                        Error::invalid(format!("cannot read icon {}: {e}", path.display()))
                    })?;
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
                    uploaded.insert(icon, r.clone());
                    r
                }
            },
        };
        refs.push(reference);
    }
    Ok(refs)
}

/// The `POST /content` body: one chapter per track, numbered from 1, so the
/// player's skip buttons move between the book's parts.
pub fn content_body(title: &str, tracks: &[Uploaded], cover_url: Option<&str>) -> Result<Value> {
    let mut chapters = Vec::with_capacity(tracks.len());
    let mut total_duration = 0.0;
    let mut total_size = 0u64;
    for (i, u) in tracks.iter().enumerate() {
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
        let label = (i + 1).to_string();
        let key = format!("{:02}", i + 1);
        let mut track = json!({
            "key": "01",
            "title": u.title,
            "trackUrl": format!("yoto:#{}", t.sha256),
            "duration": duration,
            "fileSize": file_size,
            "format": format,
            "type": "audio",
            "overlayLabel": label,
            "display": {"icon16x16": u.icon},
        });
        if let Some(ch) = &t.channels {
            track["channels"] = ch.clone();
        }
        chapters.push(json!({
            "key": key,
            "title": u.title,
            "overlayLabel": label,
            "tracks": [track],
            "display": {"icon16x16": u.icon},
        }));
    }
    let mut metadata = json!({
        "media": {
            "duration": total_duration,
            "fileSize": total_size,
            "readableFileSize": (total_size as f64 / 1024.0 / 1024.0 * 10.0).round() / 10.0,
        }
    });
    if let Some(url) = cover_url {
        metadata["cover"] = json!({"imageL": url});
    }
    Ok(json!({
        "title": title,
        "content": {"chapters": chapters},
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
            icon: DEFAULT_ICON.into(),
        }
    }

    #[test]
    fn body_has_one_numbered_chapter_per_track() {
        let body = content_body(
            "Boer Boris",
            &[
                uploaded("Een", "s1", Some(10.0)),
                uploaded("Twee", "s2", Some(20.5)),
            ],
            Some("https://cover.example/c.jpg"),
        )
        .unwrap();
        let chapters = body["content"]["chapters"].as_array().unwrap();
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[1]["key"], "02");
        assert_eq!(chapters[1]["overlayLabel"], "2");
        assert_eq!(chapters[1]["title"], "Twee");
        let t = &chapters[1]["tracks"][0];
        assert_eq!(t["trackUrl"], "yoto:#s2");
        assert_eq!(t["duration"], 20.5);
        assert_eq!(t["type"], "audio");
        assert_eq!(t["channels"], "stereo");
        assert_eq!(t["display"]["icon16x16"], DEFAULT_ICON);
        assert_eq!(chapters[1]["display"]["icon16x16"], DEFAULT_ICON);
        assert_eq!(body["metadata"]["media"]["duration"], 30.5);
        assert_eq!(body["metadata"]["media"]["fileSize"], 2_097_152);
        assert_eq!(body["metadata"]["media"]["readableFileSize"], 2.0);
        assert_eq!(
            body["metadata"]["cover"]["imageL"],
            "https://cover.example/c.jpg"
        );
    }

    #[test]
    fn missing_transcode_duration_is_an_error_not_zero() {
        let err = content_body("x", &[uploaded("Een", "s1", None)], None).unwrap_err();
        assert_eq!(err.kind(), "api_error");
        assert!(err.to_string().contains("duration"));
    }

    #[test]
    fn no_cover_means_no_cover_field() {
        let body = content_body("x", &[uploaded("Een", "s1", Some(1.0))], None).unwrap();
        assert!(body["metadata"].get("cover").is_none());
    }
}
