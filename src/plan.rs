//! Turn a folder of audio files, or one playlist of a prepared manifest, into
//! an upload plan, and check it against the Yoto limits before anything is
//! sent.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::icons::{self, IconMap};

pub const MAX_TRACKS: usize = 100;
pub const MAX_TRACK_BYTES: u64 = 100_000_000;
pub const MAX_TRACK_SECONDS: f64 = 3600.0;
pub const MAX_PLAYLIST_BYTES: u64 = 500_000_000;

/// The playlist size limit, checked again on the sizes Yoto reports after
/// transcoding, which can exceed the source files' sizes.
pub fn check_transcoded_bytes(bytes: u64) -> Result<()> {
    if bytes > MAX_PLAYLIST_BYTES {
        return Err(Error::LimitExceeded {
            message: format!(
                "once transcoded the playlist would hold {bytes} bytes, above Yoto's limit of {MAX_PLAYLIST_BYTES}; nothing was saved"
            ),
        });
    }
    Ok(())
}
pub const MAX_TITLE_CHARS: usize = 140;

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "m4b", "mp4", "aac", "wav", "flac", "ogg", "oga", "opus", "aif", "aiff", "aifc",
];
const COVER_NAMES: &[&str] = &["cover.jpg", "cover.jpeg", "cover.png"];

/// The container, detected from the file's bytes. Extensions lie: some
/// sources ship AAC-in-MP4 files named `.mp3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    Mp4,
    Wav,
    Flac,
    /// Ogg holding Opus or Vorbis; Yoto transcodes either.
    Ogg,
}

impl AudioFormat {
    pub fn content_type(self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "audio/mpeg",
            AudioFormat::Mp4 => "audio/mp4",
            AudioFormat::Wav => "audio/wav",
            AudioFormat::Flac => "audio/flac",
            AudioFormat::Ogg => "audio/ogg",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Mp4 => "m4a",
            AudioFormat::Wav => "wav",
            AudioFormat::Flac => "flac",
            AudioFormat::Ogg => "ogg",
        }
    }

    pub fn sniff(head: &[u8]) -> Option<AudioFormat> {
        if head.starts_with(b"ID3")
            || (head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0 && head[1] & 0x06 != 0)
        {
            Some(AudioFormat::Mp3)
        } else if head.len() >= 8 && &head[4..8] == b"ftyp" {
            Some(AudioFormat::Mp4)
        } else if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WAVE") {
            Some(AudioFormat::Wav)
        } else if head.starts_with(b"fLaC") {
            Some(AudioFormat::Flac)
        } else if head.starts_with(b"OggS") {
            Some(AudioFormat::Ogg)
        } else {
            None
        }
    }
}

/// AIFF and AIFF-C, which Yoto's transcoder rejects as "not a valid
/// audio/video file". Recognised only to refuse them with a clear reason.
fn is_aiff(head: &[u8]) -> bool {
    head.starts_with(b"FORM") && matches!(head.get(8..12), Some(b"AIFF") | Some(b"AIFC"))
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedTrack {
    pub path: PathBuf,
    pub title: String,
    pub bytes: u64,
    pub format: AudioFormat,
    /// From the manifest; checked against the file before upload.
    pub expected_sha256: Option<String>,
    /// From the manifest; `None` when unknown until Yoto transcodes it.
    pub duration_seconds: Option<f64>,
    /// From the manifest; `None` plays on as the playlist is set to.
    pub on_end: Option<OnEnd>,
}

/// What the player does when a track ends, instead of moving on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnEnd {
    Stop,
    Repeat,
}

impl OnEnd {
    pub fn name(self) -> &'static str {
        match self {
            OnEnd::Stop => "stop",
            OnEnd::Repeat => "repeat",
        }
    }
}

/// One chapter: what the player's skip buttons move between. It holds a
/// contiguous run of the plan's tracks.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedChapter {
    pub title: String,
    pub tracks: std::ops::Range<usize>,
}

impl PlannedChapter {
    /// Every track its own chapter, titled after the track.
    pub fn one_per_track(tracks: &[PlannedTrack]) -> Vec<PlannedChapter> {
        tracks
            .iter()
            .enumerate()
            .map(|(i, t)| PlannedChapter {
                title: t.title.clone(),
                tracks: i..i + 1,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub title: String,
    /// Every track in play order; `chapters` groups them.
    pub tracks: Vec<PlannedTrack>,
    pub chapters: Vec<PlannedChapter>,
    pub cover: Option<PathBuf>,
    /// Files in the folder that are not uploaded (not audio, or hidden).
    pub ignored: Vec<String>,
    /// Chapter icons; chapters without one get the default Yoto icon.
    pub icons: IconMap,
}

impl Plan {
    pub fn total_bytes(&self) -> u64 {
        self.tracks.iter().map(|t| t.bytes).sum()
    }

    /// Total length, or `None` if any track's length is still unknown.
    pub fn total_seconds(&self) -> Option<f64> {
        self.tracks.iter().map(|t| t.duration_seconds).sum()
    }

    /// Check every Yoto limit that can be checked locally.
    pub fn check_limits(&self) -> Result<()> {
        let chars = self.title.chars().count();
        if !self.tracks.is_empty() && (chars == 0 || chars > MAX_TITLE_CHARS) {
            return Err(Error::invalid(format!(
                "the playlist title must be 1 to {MAX_TITLE_CHARS} characters (it is {chars})"
            )));
        }
        self.check_audio_limits()
    }

    /// The limits on the audio alone, for chapters added to a playlist that
    /// keeps its own title.
    pub fn check_audio_limits(&self) -> Result<()> {
        let fail = |message: String| Err(Error::LimitExceeded { message });
        if self.tracks.is_empty() {
            return Err(Error::invalid("the playlist has no audio files"));
        }
        if self.tracks.len() > MAX_TRACKS {
            return fail(format!(
                "{} tracks; a playlist holds at most {MAX_TRACKS}",
                self.tracks.len()
            ));
        }
        for t in &self.tracks {
            if t.bytes > MAX_TRACK_BYTES {
                return fail(format!(
                    "{} is {} bytes; a track may be at most {MAX_TRACK_BYTES}",
                    t.path.display(),
                    t.bytes
                ));
            }
            if let Some(d) = t.duration_seconds.filter(|d| *d > MAX_TRACK_SECONDS) {
                return fail(format!(
                    "{} lasts {d:.0} s; a track may last at most {MAX_TRACK_SECONDS:.0} s",
                    t.path.display()
                ));
            }
        }
        let total = self.total_bytes();
        if total > MAX_PLAYLIST_BYTES {
            return fail(format!(
                "the playlist is {total} bytes; at most {MAX_PLAYLIST_BYTES} are allowed"
            ));
        }
        Ok(())
    }

    /// The 1-based number of the chapter holding track `index`.
    pub fn chapter_of(&self, index: usize) -> usize {
        self.chapters
            .iter()
            .position(|c| c.tracks.contains(&index))
            .expect("every track is in a chapter")
            + 1
    }

    /// The plan as JSON, for `--dry-run` and the upload summary.
    pub fn to_json(&self) -> Value {
        json!({
            "title": self.title,
            "chapters": self.chapters.iter().enumerate().map(|(i, c)| json!({
                "number": i + 1,
                "title": c.title,
                "track_count": c.tracks.len(),
                "icon": self.icons.for_chapter(i + 1).map(|ic| ic.describe()),
            })).collect::<Vec<_>>(),
            "chapter_count": self.chapters.len(),
            "tracks": self.tracks.iter().enumerate().map(|(i, t)| json!({
                "number": i + 1,
                "chapter": self.chapter_of(i),
                "title": t.title,
                "file": t.path.display().to_string(),
                "bytes": t.bytes,
                "format": t.format.name(),
                "duration_seconds": t.duration_seconds,
                "on_end": t.on_end.map(OnEnd::name),
                "icon": self.icons.for_chapter(self.chapter_of(i)).map(|ic| ic.describe()),
            })).collect::<Vec<_>>(),
            "track_count": self.tracks.len(),
            "bytes": self.total_bytes(),
            "duration_seconds": self.total_seconds(),
            "cover_file": self.cover.as_ref().map(|c| c.display().to_string()),
            "ignored": self.ignored,
            "icons_file": self.icons.source.as_ref().map(|p| p.display().to_string()),
        })
    }
}

fn read_head(path: &Path) -> Result<Vec<u8>> {
    let mut f = fs::File::open(path)
        .map_err(|e| Error::invalid(format!("cannot open {}: {e}", path.display())))?;
    let mut head = vec![0u8; 12];
    let n = f
        .read(&mut head)
        .map_err(|e| Error::invalid(format!("cannot read {}: {e}", path.display())))?;
    head.truncate(n);
    Ok(head)
}

fn inspect(path: &Path) -> Result<(u64, AudioFormat)> {
    let meta = fs::metadata(path)
        .map_err(|e| Error::invalid(format!("cannot read {}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(Error::invalid(format!("{} is not a file", path.display())));
    }
    let head = read_head(path)?;
    if is_aiff(&head) {
        return Err(Error::invalid(format!(
            "{} is AIFF audio, which Yoto cannot transcode; convert it to MP3, M4A, WAV or FLAC first",
            path.display()
        )));
    }
    let format = AudioFormat::sniff(&head).ok_or_else(|| {
        Error::invalid(format!(
            "{} is not MP3, MP4/AAC, WAV, FLAC or Ogg audio",
            path.display()
        ))
    })?;
    Ok((meta.len(), format))
}

/// `"01 - Title.mp3"` -> `"Title"`; names without a numeric prefix keep their stem.
pub fn title_from_filename(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    strip_number(&stem)
}

/// `"02 - Vol. 2"` -> `"Vol. 2"`: a folder name has no extension to drop.
fn title_from_dirname(name: &str) -> String {
    strip_number(name)
}

fn strip_number(stem: &str) -> String {
    if let Some((prefix, rest)) = stem.split_once(" - ")
        && !prefix.is_empty()
        && prefix.chars().all(|c| c.is_ascii_digit())
        && !rest.trim().is_empty()
    {
        return rest.trim().to_string();
    }
    stem.trim().to_string()
}

fn dir_name(dir: &Path) -> String {
    dir.canonicalize()
        .ok()
        .as_deref()
        .and_then(Path::file_name)
        .or_else(|| dir.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A `Kaart`/`Kaart 2` folder is named after its book folder; anything else after itself.
fn default_title(dir: &Path) -> String {
    let name = dir_name(dir);
    let card_suffix = name
        .strip_prefix("Kaart")
        .filter(|s| s.is_empty() || s.starts_with(' '));
    match card_suffix {
        Some(suffix) => {
            let parent = dir
                .canonicalize()
                .ok()
                .and_then(|d| d.parent().map(dir_name))
                .unwrap_or_default();
            let suffix = suffix.trim();
            if suffix.is_empty() {
                parent
            } else {
                format!("{parent} - Kaart {suffix}")
            }
        }
        None => name,
    }
}

fn find_cover(dirs: &[&Path]) -> Option<PathBuf> {
    dirs.iter()
        .flat_map(|d| COVER_NAMES.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
}

/// Which icons.json a plan takes its chapter icons from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconsFile<'a> {
    /// The one beside the audio or in the book folder, when there is one.
    Detect,
    /// This one, in place of any detected file.
    Path(&'a Path),
    /// None: every track gets the default icon.
    Ignore,
}

/// Load the chosen icons.json. A detected file is only read when nothing
/// overrides it, so a broken one can always be bypassed.
fn load_icons(choice: IconsFile, search: &[&Path], chapter_count: usize) -> Result<IconMap> {
    match choice {
        IconsFile::Detect => match icons::find(search) {
            Some(path) => IconMap::load(&path, chapter_count),
            None => Ok(IconMap::default()),
        },
        IconsFile::Path(path) => IconMap::load(path, chapter_count),
        IconsFile::Ignore => Ok(IconMap::default()),
    }
}

fn is_audio_name(name: &str) -> bool {
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    !name.starts_with('.') && AUDIO_EXTENSIONS.contains(&ext.as_str())
}

/// The entries of `dir`, split into files and folders, each in name order.
fn list_dir(dir: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let err =
        |e: std::io::Error| Error::invalid(format!("cannot read folder {}: {e}", dir.display()));
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for entry in fs::read_dir(dir).map_err(err)? {
        let entry = entry.map_err(err)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if path.is_file() {
            files.push(name);
        } else if path.is_dir() {
            dirs.push(name);
        }
    }
    files.sort();
    dirs.sort();
    Ok((files, dirs))
}

fn planned_track(path: PathBuf, title: String) -> Result<PlannedTrack> {
    let (bytes, format) = inspect(&path)?;
    Ok(PlannedTrack {
        path,
        title,
        bytes,
        format,
        expected_sha256: None,
        duration_seconds: None,
        on_end: None,
    })
}

/// Plan a playlist from `dir`, in name order: each audio file is a chapter,
/// and each subfolder is one chapter of the audio files in it.
pub fn from_dir(dir: &Path, icons_file: IconsFile) -> Result<Plan> {
    let (files, dirs) = list_dir(dir)?;
    let mut entries: Vec<(&String, bool)> = files
        .iter()
        .map(|f| (f, false))
        .chain(
            dirs.iter()
                .filter(|d| !d.starts_with('.'))
                .map(|d| (d, true)),
        )
        .collect();
    entries.sort();
    let mut tracks = Vec::new();
    let mut chapters = Vec::new();
    let mut ignored = Vec::new();
    for (name, is_dir) in entries {
        let first = tracks.len();
        if is_dir {
            let sub = dir.join(name);
            let (sub_files, sub_dirs) = list_dir(&sub)?;
            ignored.extend(
                sub_dirs
                    .iter()
                    .filter(|d| !d.starts_with('.'))
                    .map(|d| format!("{name}/{d}/")),
            );
            for f in sub_files {
                if is_audio_name(&f) {
                    tracks.push(planned_track(sub.join(&f), title_from_filename(&f))?);
                } else if !f.starts_with('.') {
                    ignored.push(format!("{name}/{f}"));
                }
            }
            if tracks.len() > first {
                chapters.push(PlannedChapter {
                    title: title_from_dirname(name),
                    tracks: first..tracks.len(),
                });
            }
            continue;
        }
        if !is_audio_name(name) {
            let lower = name.to_ascii_lowercase();
            if !name.starts_with('.')
                && !COVER_NAMES.contains(&lower.as_str())
                && lower != icons::ICONS_FILE
            {
                ignored.push(name.clone());
            }
            continue;
        }
        let title = title_from_filename(name);
        tracks.push(planned_track(dir.join(name), title.clone())?);
        chapters.push(PlannedChapter {
            title,
            tracks: first..first + 1,
        });
    }
    let parent = dir
        .canonicalize()
        .ok()
        .and_then(|d| d.parent().map(Path::to_path_buf));
    let mut search = vec![dir];
    if let Some(p) = parent.as_deref() {
        search.push(p);
    }
    let icons = load_icons(icons_file, &search, chapters.len())?;
    Ok(Plan {
        title: default_title(dir),
        tracks,
        chapters,
        cover: find_cover(&search),
        ignored,
        icons,
    })
}

#[derive(Deserialize)]
struct Manifest {
    books: std::collections::BTreeMap<String, ManifestBook>,
}

#[derive(Deserialize)]
struct ManifestBook {
    folder: String,
    #[serde(default)]
    covers: Vec<String>,
    playlists: Vec<ManifestPlaylist>,
}

#[derive(Deserialize)]
struct ManifestPlaylist {
    name: String,
    entries: Vec<ManifestEntry>,
}

#[derive(Deserialize)]
struct ManifestEntry {
    file: String,
    title: String,
    bytes: u64,
    sha256: String,
    #[serde(default)]
    duration_seconds: Option<f64>,
    /// Consecutive entries naming the same chapter play as one chapter.
    #[serde(default)]
    chapter: Option<String>,
    #[serde(default)]
    on_end: Option<OnEnd>,
}

/// Plan one playlist of a prepared book from `manifest.json`. Files resolve
/// relative to the manifest's folder; `playlist` may be omitted when the
/// book has a single one.
pub fn from_manifest(
    manifest_path: &Path,
    book: &str,
    playlist: Option<&str>,
    icons_file: IconsFile,
) -> Result<Plan> {
    let text = fs::read_to_string(manifest_path)
        .map_err(|e| Error::invalid(format!("cannot read {}: {e}", manifest_path.display())))?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|e| {
        Error::invalid(format!(
            "{} is not a valid manifest: {e}",
            manifest_path.display()
        ))
    })?;
    let Some(b) = manifest.books.get(book) else {
        let keys: Vec<_> = manifest.books.keys().map(String::as_str).collect();
        return Err(Error::invalid(format!(
            "no book {book:?} in the manifest; books: {}",
            keys.join(", ")
        )));
    };
    let names = || {
        b.playlists
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let pl = match playlist {
        Some(name) => b.playlists.iter().find(|p| p.name == name).ok_or_else(|| {
            Error::invalid(format!(
                "book {book:?} has no playlist {name:?}; playlists: {}",
                names()
            ))
        })?,
        None if b.playlists.len() == 1 => &b.playlists[0],
        None => {
            return Err(Error::usage(format!(
                "book {book:?} has {} playlists; choose one with --playlist ({})",
                b.playlists.len(),
                names()
            )));
        }
    };
    let root = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(&b.folder);
    let mut tracks = Vec::new();
    let mut chapters: Vec<PlannedChapter> = Vec::new();
    let mut chapter_names: Vec<Option<&str>> = Vec::new();
    for (i, e) in pl.entries.iter().enumerate() {
        let name = e.chapter.as_deref();
        match chapters.last_mut() {
            Some(last) if name.is_some() && chapter_names.last() == Some(&name) => {
                last.tracks.end = i + 1;
            }
            _ => {
                if name.is_some() && chapter_names.contains(&name) {
                    return Err(Error::invalid(format!(
                        "{}: chapter {:?} is split by other entries; its entries must follow each other",
                        manifest_path.display(),
                        name.unwrap_or_default()
                    )));
                }
                chapters.push(PlannedChapter {
                    title: name.unwrap_or(&e.title).to_string(),
                    tracks: i..i + 1,
                });
                chapter_names.push(name);
            }
        }
        let path = root.join(&e.file);
        let (bytes, format) = inspect(&path)?;
        if bytes != e.bytes {
            return Err(Error::invalid(format!(
                "{} is {bytes} bytes but the manifest says {}",
                path.display(),
                e.bytes
            )));
        }
        tracks.push(PlannedTrack {
            path,
            title: e.title.clone(),
            bytes,
            format,
            expected_sha256: Some(e.sha256.to_ascii_lowercase()),
            duration_seconds: e.duration_seconds,
            on_end: e.on_end,
        });
    }
    let title = if b.playlists.len() == 1 {
        b.folder.clone()
    } else {
        format!("{} - {}", b.folder, pl.name)
    };
    let cover = b.covers.iter().map(|c| root.join(c)).find(|p| p.is_file());
    // An icons.json beside the playlist's files wins over one for the whole book.
    let playlist_dir = tracks
        .first()
        .and_then(|t| t.path.parent())
        .filter(|d| *d != root);
    let search: Vec<&Path> = playlist_dir.into_iter().chain([root.as_path()]).collect();
    let icons = load_icons(icons_file, &search, chapters.len())?;
    Ok(Plan {
        title,
        tracks,
        chapters,
        cover,
        ignored: Vec::new(),
        icons,
    })
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)
        .map_err(|e| Error::invalid(format!("cannot open {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| Error::invalid(format!("cannot read {}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Hash every track (Yoto keys uploads by SHA-256) and check manifest hashes.
pub fn hash_tracks(
    plan: &Plan,
    mut progress: impl FnMut(usize, &PlannedTrack),
) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(plan.tracks.len());
    for (i, t) in plan.tracks.iter().enumerate() {
        progress(i, t);
        let sha = sha256_file(&t.path)?;
        if let Some(expected) = &t.expected_sha256
            && expected != &sha
        {
            return Err(Error::invalid(format!(
                "{} does not match the manifest (sha256 {sha}, expected {expected})",
                t.path.display()
            )));
        }
        out.push(sha);
    }
    Ok(out)
}

/// The image MIME type of a cover, from its bytes.
pub fn cover_content_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MP3: &[u8] = b"ID3\x04\x00\x00\x00\x00\x00\x00rest";
    const M4A: &[u8] = b"\x00\x00\x00\x20ftypM4A rest";

    #[test]
    fn sniffs_containers_not_extensions() {
        assert_eq!(AudioFormat::sniff(MP3), Some(AudioFormat::Mp3));
        assert_eq!(
            AudioFormat::sniff(&[0xFF, 0xFB, 0x90, 0x00]),
            Some(AudioFormat::Mp3)
        );
        assert_eq!(AudioFormat::sniff(M4A), Some(AudioFormat::Mp4));
        assert_eq!(AudioFormat::sniff(b"OggS\x00\x02"), Some(AudioFormat::Ogg));
        assert_eq!(
            AudioFormat::sniff(b"RIFF\x24\x08\x00\x00WAVEfmt "),
            Some(AudioFormat::Wav)
        );
        assert_eq!(AudioFormat::sniff(b"RIFF\x24\x08\x00\x00AVI LIST"), None);
        assert_eq!(
            AudioFormat::sniff(b"fLaC\x00\x00\x00\x22"),
            Some(AudioFormat::Flac)
        );
        assert_eq!(AudioFormat::sniff(b"FORM\x00\x00\x10\x00AIFFCOMM"), None);
        assert!(is_aiff(b"FORM\x00\x00\x10\x00AIFFCOMM"));
        assert!(is_aiff(b"FORM\x00\x00\x10\x00AIFCFVER"));
        assert!(!is_aiff(b"FORM\x00\x00\x10\x00ILBMBMHD"));
        assert_eq!(AudioFormat::sniff(b""), None);
        assert_eq!(AudioFormat::sniff(&[0xFF, 0xE0]), None);
    }

    #[test]
    fn titles_drop_only_a_numeric_prefix() {
        assert_eq!(
            title_from_filename("01 - Boer Boris en de olifant.m4a"),
            "Boer Boris en de olifant"
        );
        assert_eq!(
            title_from_filename("6.01 Op De Drempel.mp3"),
            "6.01 Op De Drempel"
        );
        assert_eq!(
            title_from_filename("Hoofdstuk - Een.mp3"),
            "Hoofdstuk - Een"
        );
        assert_eq!(title_from_filename("07 - .mp3"), "07 -");
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn dir_plan_orders_by_name_detects_format_and_finds_parent_cover() {
        let tmp = tempfile::tempdir().unwrap();
        let book = tmp.path().join("Boer Boris - Ted van Lieshout");
        let card = book.join("Kaart");
        write(&card.join("02 - Tweede.mp3"), M4A);
        write(&card.join("01 - Eerste.mp3"), MP3);
        write(&card.join(".DS_Store"), b"x");
        write(&card.join("notes.txt"), b"x");
        write(&book.join("cover.png"), b"\x89PNG\r\n\x1a\nxx");
        let plan = from_dir(&card, IconsFile::Detect).unwrap();
        assert_eq!(plan.title, "Boer Boris - Ted van Lieshout");
        let titles: Vec<_> = plan.tracks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["Eerste", "Tweede"]);
        assert_eq!(plan.tracks[1].format, AudioFormat::Mp4);
        assert_eq!(
            plan.cover.as_deref().map(|p| p.file_name().unwrap()),
            Some("cover.png".as_ref())
        );
        assert_eq!(plan.ignored, ["notes.txt"]);
        assert_eq!(plan.total_seconds(), None);
    }

    #[test]
    fn aiff_is_refused_before_anything_is_sent() {
        // Yoto's transcoder rejects AIFF ("not a valid audio/video file"), so
        // the folder is refused here instead of after a wasted upload.
        let tmp = tempfile::tempdir().unwrap();
        let card = tmp.path().join("Kaart");
        write(&card.join("01 - Eerste.mp3"), MP3);
        write(
            &card.join("02 - Tweede.aif"),
            b"FORM\x00\x00\x10\x00AIFFCOMM",
        );
        let err = from_dir(&card, IconsFile::Detect).unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
        let message = err.to_string();
        assert!(message.contains("02 - Tweede.aif"), "{message}");
        assert!(message.contains("AIFF"), "{message}");
        assert!(message.contains("convert"), "{message}");
    }

    #[test]
    fn a_subfolder_is_one_chapter_of_its_tracks_in_name_order() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Liedjes");
        write(&dir.join("01 - Intro.mp3"), MP3);
        write(&dir.join("02 - Vol. 2/02 - Tweede.mp3"), MP3);
        write(&dir.join("02 - Vol. 2/01 - Eerste.m4a"), M4A);
        write(&dir.join("02 - Vol. 2/hoes.txt"), b"x");
        write(&dir.join("03 - Leeg/notes.txt"), b"x");
        write(&dir.join(".hidden/01.mp3"), MP3);
        write(&dir.join("04 - Slot.mp3"), MP3);
        let plan = from_dir(&dir, IconsFile::Detect).unwrap();
        let titles: Vec<_> = plan.tracks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["Intro", "Eerste", "Tweede", "Slot"]);
        let chapters: Vec<_> = plan
            .chapters
            .iter()
            .map(|c| (c.title.as_str(), c.tracks.clone()))
            .collect();
        assert_eq!(
            chapters,
            [("Intro", 0..1), ("Vol. 2", 1..3), ("Slot", 3..4)]
        );
        assert_eq!(
            plan.ignored,
            ["02 - Vol. 2/hoes.txt", "03 - Leeg/notes.txt"]
        );
        let json = plan.to_json();
        assert_eq!(json["chapter_count"], 3);
        assert_eq!(json["tracks"][2]["chapter"], 2);
        assert_eq!(json["chapters"][1]["title"], "Vol. 2");
        assert_eq!(json["chapters"][1]["track_count"], 2);
    }

    #[test]
    fn deeper_folders_are_reported_not_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Boek");
        write(&dir.join("01 - Deel/01.mp3"), MP3);
        write(&dir.join("01 - Deel/CD2/01.mp3"), MP3);
        write(&dir.join("01 - Deel/.cache/x"), b"x");
        let plan = from_dir(&dir, IconsFile::Detect).unwrap();
        assert_eq!(plan.tracks.len(), 1);
        assert_eq!(plan.ignored, ["01 - Deel/CD2/"]);
    }

    #[test]
    fn icons_are_numbered_by_chapter() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Liedjes");
        write(&dir.join("01 - Deel/01.mp3"), MP3);
        write(&dir.join("01 - Deel/02.mp3"), MP3);
        write(&dir.join("02 - Slot.mp3"), MP3);
        let id = "7Wt--qhx9t7PCSC8WBmGKJvlG53Z1uk7bN1dE9wGWJ0";
        write(
            &dir.join(icons::ICONS_FILE),
            json!({"2": format!("yoto:#{id}")}).to_string().as_bytes(),
        );
        let plan = from_dir(&dir, IconsFile::Detect).unwrap();
        assert_eq!(
            plan.icons.for_chapter(2),
            Some(&icons::Icon::Yoto(id.into()))
        );
        write(
            &dir.join(icons::ICONS_FILE),
            json!({"3": format!("yoto:#{id}")}).to_string().as_bytes(),
        );
        assert_eq!(
            from_dir(&dir, IconsFile::Detect).unwrap_err().kind(),
            "invalid_input"
        );
    }

    #[test]
    fn numbered_card_folder_gets_a_card_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let card = tmp.path().join("De Hobbit").join("Kaart 2");
        write(&card.join("01 - a.mp3"), MP3);
        assert_eq!(
            from_dir(&card, IconsFile::Detect).unwrap().title,
            "De Hobbit - Kaart 2"
        );
        let other = tmp.path().join("Kaartspel");
        write(&other.join("01 - a.mp3"), MP3);
        assert_eq!(
            from_dir(&other, IconsFile::Detect).unwrap().title,
            "Kaartspel"
        );
    }

    #[test]
    fn non_audio_with_audio_extension_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("01 - a.mp3"), b"OggS\x00\x02....");
        let err = from_dir(tmp.path(), IconsFile::Detect).unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
    }

    fn track(bytes: u64, secs: Option<f64>) -> PlannedTrack {
        PlannedTrack {
            path: "t.mp3".into(),
            title: "t".into(),
            bytes,
            format: AudioFormat::Mp3,
            expected_sha256: None,
            duration_seconds: secs,
            on_end: None,
        }
    }

    fn plan(tracks: Vec<PlannedTrack>) -> Plan {
        Plan {
            title: "x".into(),
            chapters: PlannedChapter::one_per_track(&tracks),
            tracks,
            cover: None,
            ignored: vec![],
            icons: IconMap::default(),
        }
    }

    #[test]
    fn limits_are_inclusive_at_the_boundary() {
        assert!(
            plan(vec![track(MAX_TRACK_BYTES, Some(MAX_TRACK_SECONDS))])
                .check_limits()
                .is_ok()
        );
        assert_eq!(
            plan(vec![track(MAX_TRACK_BYTES + 1, None)])
                .check_limits()
                .unwrap_err()
                .kind(),
            "limit_exceeded"
        );
        assert_eq!(
            plan(vec![track(1, Some(3600.5))])
                .check_limits()
                .unwrap_err()
                .kind(),
            "limit_exceeded"
        );
        assert!(
            plan(vec![track(1, None); MAX_TRACKS])
                .check_limits()
                .is_ok()
        );
        assert_eq!(
            plan(vec![track(1, None); MAX_TRACKS + 1])
                .check_limits()
                .unwrap_err()
                .kind(),
            "limit_exceeded"
        );
        assert!(
            plan(vec![track(MAX_PLAYLIST_BYTES / 5, None); 5])
                .check_limits()
                .is_ok()
        );
        let over = vec![
            track(MAX_PLAYLIST_BYTES / 5, None),
            track(MAX_PLAYLIST_BYTES / 5, None),
            track(MAX_PLAYLIST_BYTES / 5, None),
            track(MAX_PLAYLIST_BYTES / 5, None),
            track(MAX_PLAYLIST_BYTES / 5 + 1, None),
        ];
        assert_eq!(
            plan(over).check_limits().unwrap_err().kind(),
            "limit_exceeded"
        );
        assert_eq!(
            plan(vec![]).check_limits().unwrap_err().kind(),
            "invalid_input"
        );
        let mut long = plan(vec![track(1, None)]);
        long.title = "x".repeat(MAX_TITLE_CHARS + 1);
        assert_eq!(long.check_limits().unwrap_err().kind(), "invalid_input");
    }

    fn manifest_fixture(tmp: &Path, playlists: Value) -> PathBuf {
        let m = json!({"books": {"bb": {"folder": "Boer Boris", "covers": ["cover.png"], "playlists": playlists}}});
        let p = tmp.join("manifest.json");
        fs::write(&p, serde_json::to_vec(&m).unwrap()).unwrap();
        p
    }

    fn entry(file: &str, title: &str, bytes: &[u8]) -> Value {
        json!({"file": file, "title": title, "bytes": bytes.len(), "duration_seconds": 12.5,
               "sha256": format!("{:x}", Sha256::digest(bytes))})
    }

    #[test]
    fn manifest_plan_uses_titles_order_and_checks_hashes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Boer Boris");
        write(&root.join("Kaart/01 - b.m4a"), M4A);
        write(&root.join("Kaart/02 - a.mp3"), MP3);
        write(&root.join("cover.png"), b"\x89PNG\r\n\x1a\n");
        let m = manifest_fixture(
            tmp.path(),
            json!([{"name": "Kaart", "entries": [
                entry("Kaart/01 - b.m4a", "Boer Boris en de olifant", M4A),
                entry("Kaart/02 - a.mp3", "Tweede", MP3),
            ]}]),
        );
        let plan = from_manifest(&m, "bb", None, IconsFile::Detect).unwrap();
        assert_eq!(plan.title, "Boer Boris");
        assert_eq!(plan.tracks[0].title, "Boer Boris en de olifant");
        assert_eq!(plan.total_seconds(), Some(25.0));
        assert!(plan.cover.is_some());
        assert_eq!(hash_tracks(&plan, |_, _| {}).unwrap().len(), 2);

        // Same size, different content: the hash check must catch it.
        write(
            &root.join("Kaart/02 - a.mp3"),
            b"ID3\x04\x00\x00\x00\x00\x00\x00resT",
        );
        let plan = from_manifest(&m, "bb", None, IconsFile::Detect).unwrap();
        assert_eq!(
            hash_tracks(&plan, |_, _| {}).unwrap_err().kind(),
            "invalid_input"
        );
    }

    #[test]
    fn manifest_entries_sharing_a_chapter_are_grouped_and_keep_their_end() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Boer Boris");
        for f in ["a", "b", "c", "d", "e"] {
            write(&root.join(format!("{f}.mp3")), MP3);
        }
        let with = |file: &str, title: &str, extra: Value| {
            let mut e = entry(file, title, MP3);
            e.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            e
        };
        let m = manifest_fixture(
            tmp.path(),
            json!([{"name": "Kaart", "entries": [
                with("a.mp3", "Intro", json!({})),
                with("b.mp3", "Couplet", json!({"chapter": "Lied"})),
                with("c.mp3", "Refrein", json!({"chapter": "Lied", "on_end": "repeat"})),
                with("d.mp3", "Welterusten", json!({"on_end": "stop"})),
                with("e.mp3", "Toegift", json!({})),
            ]}]),
        );
        let plan = from_manifest(&m, "bb", None, IconsFile::Detect).unwrap();
        let chapters: Vec<_> = plan
            .chapters
            .iter()
            .map(|c| (c.title.as_str(), c.tracks.clone()))
            .collect();
        assert_eq!(
            chapters,
            [
                ("Intro", 0..1),
                ("Lied", 1..3),
                ("Welterusten", 3..4),
                // Entries without a chapter are one chapter each, even side by side.
                ("Toegift", 4..5)
            ]
        );
        let ends: Vec<_> = plan.tracks.iter().map(|t| t.on_end).collect();
        assert_eq!(
            ends,
            [None, None, Some(OnEnd::Repeat), Some(OnEnd::Stop), None]
        );
        assert_eq!(plan.to_json()["tracks"][2]["on_end"], "repeat");

        // A chapter name used again after another chapter is a mistake, not a merge.
        let split = manifest_fixture(
            tmp.path(),
            json!([{"name": "Kaart", "entries": [
                with("a.mp3", "a", json!({"chapter": "Lied"})),
                with("b.mp3", "b", json!({})),
                with("c.mp3", "c", json!({"chapter": "Lied"})),
            ]}]),
        );
        assert_eq!(
            from_manifest(&split, "bb", None, IconsFile::Detect)
                .unwrap_err()
                .kind(),
            "invalid_input"
        );
        let bad = manifest_fixture(
            tmp.path(),
            json!([{"name": "Kaart", "entries": [with("a.mp3", "a", json!({"on_end": "loop"}))]}]),
        );
        assert_eq!(
            from_manifest(&bad, "bb", None, IconsFile::Detect)
                .unwrap_err()
                .kind(),
            "invalid_input"
        );
    }

    #[test]
    fn manifest_with_several_playlists_needs_a_choice() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Boer Boris");
        write(&root.join("Kaart 1/01.mp3"), MP3);
        write(&root.join("Kaart 2/01.mp3"), MP3);
        let m = manifest_fixture(
            tmp.path(),
            json!([
                {"name": "Kaart 1", "entries": [entry("Kaart 1/01.mp3", "a", MP3)]},
                {"name": "Kaart 2", "entries": [entry("Kaart 2/01.mp3", "b", MP3)]},
            ]),
        );
        assert_eq!(
            from_manifest(&m, "bb", None, IconsFile::Detect)
                .unwrap_err()
                .kind(),
            "usage"
        );
        let plan = from_manifest(&m, "bb", Some("Kaart 2"), IconsFile::Detect).unwrap();
        assert_eq!(plan.title, "Boer Boris - Kaart 2");
        assert_eq!(plan.tracks[0].title, "b");
        assert_eq!(
            from_manifest(&m, "bb", Some("Kaart 3"), IconsFile::Detect)
                .unwrap_err()
                .kind(),
            "invalid_input"
        );
        assert_eq!(
            from_manifest(&m, "nope", None, IconsFile::Detect)
                .unwrap_err()
                .kind(),
            "invalid_input"
        );
    }

    #[test]
    fn manifest_size_mismatch_is_caught_before_hashing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Boer Boris");
        write(&root.join("Kaart/01.mp3"), MP3);
        let mut e = entry("Kaart/01.mp3", "a", MP3);
        e["bytes"] = json!(1);
        let m = manifest_fixture(tmp.path(), json!([{"name": "Kaart", "entries": [e]}]));
        assert_eq!(
            from_manifest(&m, "bb", None, IconsFile::Detect)
                .unwrap_err()
                .kind(),
            "invalid_input"
        );
    }

    #[test]
    fn cover_type_from_bytes() {
        assert_eq!(
            cover_content_type(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(cover_content_type(b"\x89PNG\r\n\x1a\n"), Some("image/png"));
        assert_eq!(cover_content_type(b"GIF89a"), None);
    }
}
