//! Native lightweight EBML parser for Matroska containers.
//!
//! Parses Segment Header, Tracks (video, audio, subtitles), Attachments
//! (fonts, covers), and Chapters directly in pure Rust without external process spawning.
//! A truncated tail or a malformed SeekHead entry ends the scan without failing. Any error
//! (not an EBML/Matroska file, a Cluster before any SeekHead, a malformed Tracks or
//! Attachments element, no track found, I/O failure) makes the caller fall back to
//! `mkvmerge -J` (ADR 0006).

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::domain::{lang_mkv_tag, MkvTrack, TrackProperties};
use crate::scan::{MkvAttachment, MkvChapter, MkvmergeMetadata};

// EBML and Matroska IDs
const ID_EBML_HEADER: u32 = 0x1A45DFA3;
const ID_SEGMENT: u32 = 0x18538067;
const ID_SEEK_HEAD: u32 = 0x114D9B74;
const ID_SEEK: u32 = 0x4DBB;
const ID_SEEK_ID: u32 = 0x53AB;
const ID_SEEK_POSITION: u32 = 0x53AC;
const ID_CLUSTER: u32 = 0x1F43B675;

const ID_TRACKS: u32 = 0x1654AE6B;
const ID_TRACK_ENTRY: u32 = 0xAE;
const ID_TRACK_NUMBER: u32 = 0xD7;
const ID_TRACK_TYPE: u32 = 0x83;
const ID_FLAG_DEFAULT: u32 = 0x88;
const ID_FLAG_FORCED: u32 = 0x55AA;
const ID_CODEC_ID: u32 = 0x86;
const ID_LANGUAGE: u32 = 0x22B59C;
const ID_LANGUAGE_IETF: u32 = 0x22B59D;
const ID_TRACK_NAME: u32 = 0x536E;
const ID_VIDEO: u32 = 0xE0;
const ID_PIXEL_WIDTH: u32 = 0xB0;
const ID_PIXEL_HEIGHT: u32 = 0xBA;
const ID_AUDIO: u32 = 0xE1;
const ID_CHANNELS: u32 = 0x9F;

const ID_ATTACHMENTS: u32 = 0x1941A469;
const ID_ATTACHED_FILE: u32 = 0x61A7;
const ID_FILE_NAME: u32 = 0x466E;
const ID_FILE_MIME: u32 = 0x4660;
const ID_FILE_DATA: u32 = 0x465C;

const ID_CHAPTERS: u32 = 0x1043A770;

const MAX_STRING_LEN: u64 = 1024 * 1024;

fn invalid_data(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn with_context(e: io::Error, context: &str) -> io::Error {
    io::Error::new(e.kind(), format!("{context}: {e}"))
}

/// Absolute end offset of an element whose `len`-byte payload starts at the reader's position.
fn element_end<R: Seek>(reader: &mut R, len: u64, overflow_msg: &'static str) -> io::Result<u64> {
    reader
        .stream_position()?
        .checked_add(len)
        .ok_or_else(|| invalid_data(overflow_msg))
}

/// Seeks forward by `len` bytes, rejecting lengths that do not fit a non-negative `i64`.
fn seek_forward<R: Read + Seek>(reader: &mut R, len: u64) -> io::Result<()> {
    let offset =
        i64::try_from(len).map_err(|_| invalid_data("EBML element length exceeds i64::MAX"))?;
    reader.seek(SeekFrom::Current(offset))?;
    Ok(())
}

/// Reads an EBML Element ID as a raw u32.
fn read_element_id<R: Read>(reader: &mut R) -> io::Result<u32> {
    let mut first_byte = [0u8; 1];
    reader.read_exact(&mut first_byte)?;
    let b = first_byte[0];
    if b == 0 {
        return Err(invalid_data("Invalid zero EBML ID"));
    }

    let len = b.leading_zeros() as usize + 1;
    if len > 4 {
        return Err(invalid_data("EBML ID too large (> 4 bytes)"));
    }

    let mut id = b as u32;
    for _ in 1..len {
        let mut next = [0u8; 1];
        reader.read_exact(&mut next)?;
        id = (id << 8) | (next[0] as u32);
    }
    Ok(id)
}

/// Reads an EBML Variable Size Integer (VINT) representing length.
fn read_vint_length<R: Read>(reader: &mut R) -> io::Result<u64> {
    let mut first_byte = [0u8; 1];
    reader.read_exact(&mut first_byte)?;
    let b = first_byte[0];
    if b == 0 {
        return Err(invalid_data("Invalid zero VINT length"));
    }

    let len = b.leading_zeros() as usize + 1;
    let mask = if len == 8 { 0 } else { 0xFF >> len };
    let mut value = (b & mask) as u64;
    for _ in 1..len {
        let mut next = [0u8; 1];
        reader.read_exact(&mut next)?;
        value = (value << 8) | (next[0] as u64);
    }
    let unknown_mask = if len == 8 {
        0x00FF_FFFF_FFFF_FFFF
    } else {
        (1u64 << (7 * len)) - 1
    };
    if value == unknown_mask {
        return Err(invalid_data("Unknown EBML element length"));
    }
    Ok(value)
}

fn read_uint<R: Read>(reader: &mut R, size: u64) -> io::Result<u64> {
    if size > 8 {
        return Err(invalid_data("Uint size > 8"));
    }
    let mut val = 0u64;
    for _ in 0..size {
        let mut b = [0u8; 1];
        reader.read_exact(&mut b)?;
        val = (val << 8) | (b[0] as u64);
    }
    Ok(val)
}

fn read_string<R: Read>(reader: &mut R, size: u64) -> io::Result<String> {
    if size > MAX_STRING_LEN {
        return Err(invalid_data("String too long (> 1MB)"));
    }
    let mut buf = vec![0u8; size as usize];
    reader.read_exact(&mut buf)?;
    while buf.last() == Some(&0) {
        buf.pop();
    }
    Ok(String::from_utf8(buf)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// Native EBML probe for Matroska files. Returns `Err` when the file is not Matroska, a
/// Cluster precedes any SeekHead, a Tracks or Attachments element is malformed, no track is
/// found, or I/O fails; `probe_video` then falls back to `mkvmerge -J`. A truncated tail or a
/// malformed SeekHead entry ends the scan without failing.
pub fn probe_ebml_native(path: &Path) -> Result<MkvmergeMetadata, String> {
    probe(path).map_err(|e| e.to_string())
}

/// Top-level element positions a SeekHead points at, relative to the Segment payload.
#[derive(Default)]
struct SeekTargets {
    tracks: Option<u64>,
    attachments: Option<u64>,
    chapters: bool,
}

fn probe(path: &Path) -> io::Result<MkvmergeMetadata> {
    let mut reader = BufReader::new(File::open(path)?);

    if read_element_id(&mut reader)? != ID_EBML_HEADER {
        return Err(invalid_data("Not an EBML file"));
    }
    let ebml_len = read_vint_length(&mut reader)?;
    seek_forward(&mut reader, ebml_len)?;

    if read_element_id(&mut reader)? != ID_SEGMENT {
        return Err(invalid_data("Segment element not found after EBML header"));
    }
    let _segment_len = read_vint_length(&mut reader)?;
    let segment_data_start = reader.stream_position()?;

    let mut tracks = Vec::new();
    let mut attachments = Vec::new();
    let mut has_chapters = false;
    let mut seek_targets = SeekTargets::default();
    let mut has_seek_head = false;

    // Scan top-level elements up to the first Cluster.
    loop {
        let elem_id = match read_element_id(&mut reader) {
            Ok(id) => id,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(with_context(e, "Error reading element ID")),
        };
        let elem_len = match read_vint_length(&mut reader) {
            Ok(len) => len,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(with_context(e, "Error reading element length")),
        };

        match elem_id {
            ID_SEEK_HEAD => {
                has_seek_head = true;
                let end = element_end(&mut reader, elem_len, "SeekHead element length overflow")?;
                read_seek_head(&mut reader, end, &mut seek_targets)?;
                let _ = reader.seek(SeekFrom::Start(end));
            }
            ID_TRACKS => {
                let end = element_end(&mut reader, elem_len, "Tracks block length overflow")?;
                parse_tracks_block(&mut reader, elem_len, &mut tracks)?;
                reader.seek(SeekFrom::Start(end))?;
            }
            ID_ATTACHMENTS => {
                let end = element_end(&mut reader, elem_len, "Attachments block length overflow")?;
                parse_attachments_block(&mut reader, elem_len, &mut attachments)?;
                reader.seek(SeekFrom::Start(end))?;
            }
            ID_CHAPTERS => {
                has_chapters = true;
                let _ = seek_forward(&mut reader, elem_len);
            }
            ID_CLUSTER => {
                // Anything after the clusters is reachable only through a SeekHead.
                if !has_seek_head {
                    return Err(invalid_data(
                        "No SeekHead before clusters, falling back to mkvmerge -J",
                    ));
                }
                break;
            }
            _ => {
                let _ = seek_forward(&mut reader, elem_len);
            }
        }
    }

    if tracks.is_empty() {
        if let Some(pos) = seek_targets.tracks {
            let (id, len) = element_at(
                &mut reader,
                segment_data_start,
                pos,
                "SeekHead tracks position overflow",
            )?;
            if id == ID_TRACKS {
                parse_tracks_block(&mut reader, len, &mut tracks)?;
            }
        }
    }

    if attachments.is_empty() {
        if let Some(pos) = seek_targets.attachments {
            let (id, len) = element_at(
                &mut reader,
                segment_data_start,
                pos,
                "SeekHead attachments position overflow",
            )?;
            if id == ID_ATTACHMENTS {
                parse_attachments_block(&mut reader, len, &mut attachments)?;
            }
        }
    }

    if tracks.is_empty() {
        return Err(invalid_data("No tracks discovered via native EBML probe"));
    }

    let chapters = if has_chapters || seek_targets.chapters {
        vec![MkvChapter {
            num_entries: Some(1),
        }]
    } else {
        Vec::new()
    };

    Ok(MkvmergeMetadata {
        tracks,
        attachments,
        chapters,
    })
}

/// Records the SeekHead's Tracks and Attachments positions and whether it lists Chapters.
/// A malformed entry ends the scan early instead of failing the probe.
fn read_seek_head<R: Read + Seek>(
    reader: &mut R,
    end: u64,
    targets: &mut SeekTargets,
) -> io::Result<()> {
    while reader.stream_position()? < end {
        let Ok(entry_id) = read_element_id(reader) else {
            break;
        };
        let Ok(entry_len) = read_vint_length(reader) else {
            break;
        };
        if entry_id != ID_SEEK {
            let _ = seek_forward(reader, entry_len);
            continue;
        }

        let entry_end = element_end(reader, entry_len, "Seek entry length overflow")?;
        let mut target_id: Option<u32> = None;
        let mut target_pos: Option<u64> = None;
        while reader.stream_position()? < entry_end {
            let Ok(child) = read_element_id(reader) else {
                break;
            };
            let Ok(child_len) = read_vint_length(reader) else {
                break;
            };
            let child_end = element_end(reader, child_len, "Seek child length overflow")?;
            match child {
                ID_SEEK_ID => target_id = read_element_id(reader).ok(),
                ID_SEEK_POSITION => target_pos = read_uint(reader, child_len).ok(),
                _ => {}
            }
            let _ = reader.seek(SeekFrom::Start(child_end));
        }
        if let (Some(id), Some(pos)) = (target_id, target_pos) {
            match id {
                ID_TRACKS => targets.tracks = Some(pos),
                ID_ATTACHMENTS => targets.attachments = Some(pos),
                ID_CHAPTERS => targets.chapters = true,
                _ => {}
            }
        }
        let _ = reader.seek(SeekFrom::Start(entry_end));
    }
    Ok(())
}

/// Seeks to the element `pos` bytes into the Segment payload and reads its header.
fn element_at<R: Read + Seek>(
    reader: &mut R,
    segment_data_start: u64,
    pos: u64,
    overflow_msg: &'static str,
) -> io::Result<(u32, u64)> {
    let target = segment_data_start
        .checked_add(pos)
        .ok_or_else(|| invalid_data(overflow_msg))?;
    reader.seek(SeekFrom::Start(target))?;
    Ok((read_element_id(reader)?, read_vint_length(reader)?))
}

/// Appends each TrackEntry, numbered sequentially from `tracks.len()` as mkvmerge does.
fn parse_tracks_block<R: Read + Seek>(
    reader: &mut R,
    len: u64,
    tracks: &mut Vec<MkvTrack>,
) -> io::Result<()> {
    let end = element_end(reader, len, "Tracks block length overflow")?;
    let mut track_idx = tracks.len() as u32;
    while reader.stream_position()? < end {
        let entry_id = read_element_id(reader)?;
        let entry_len = read_vint_length(reader)?;
        let entry_end = element_end(reader, entry_len, "Track entry length overflow")?;
        if entry_id == ID_TRACK_ENTRY {
            let mut track = parse_track_entry(reader, entry_len)?;
            track.id = Some(track_idx);
            track_idx += 1;
            tracks.push(track);
        }
        reader.seek(SeekFrom::Start(entry_end))?;
    }
    Ok(())
}

/// Appends each AttachedFile, numbered sequentially from 1 as mkvmerge does.
fn parse_attachments_block<R: Read + Seek>(
    reader: &mut R,
    len: u64,
    attachments: &mut Vec<MkvAttachment>,
) -> io::Result<()> {
    let end = element_end(reader, len, "Attachments block length overflow")?;
    let mut att_id = (attachments.len() + 1) as u32;
    while reader.stream_position()? < end {
        let file_id = read_element_id(reader)?;
        let file_len = read_vint_length(reader)?;
        let entry_end = element_end(reader, file_len, "Attached file length overflow")?;
        if file_id == ID_ATTACHED_FILE {
            let mut att = parse_attached_file(reader, file_len)?;
            att.id = Some(att_id);
            att_id += 1;
            attachments.push(att);
        }
        reader.seek(SeekFrom::Start(entry_end))?;
    }
    Ok(())
}

/// Parses one TrackEntry. The returned track has no `id`; the caller numbers it.
fn parse_track_entry<R: Read + Seek>(reader: &mut R, len: u64) -> io::Result<MkvTrack> {
    let end = element_end(reader, len, "Track entry length overflow")?;

    let mut track_type_num: Option<u64> = None;
    let mut codec_id: Option<String> = None;
    let mut language: Option<String> = None;
    let mut language_ietf: Option<String> = None;
    let mut track_name: Option<String> = None;
    let mut pixel_width: Option<u32> = None;
    let mut pixel_height: Option<u32> = None;
    let mut channels: Option<u32> = None;

    while reader.stream_position()? < end {
        let child_id = read_element_id(reader)?;
        let child_len = read_vint_length(reader)?;

        match child_id {
            ID_TRACK_TYPE => {
                track_type_num = Some(read_uint(reader, child_len)?);
            }
            ID_CODEC_ID => {
                codec_id = Some(read_string(reader, child_len)?);
            }
            ID_LANGUAGE => {
                language = Some(read_string(reader, child_len)?);
            }
            ID_LANGUAGE_IETF => {
                language_ietf = Some(read_string(reader, child_len)?);
            }
            ID_TRACK_NAME => {
                track_name = Some(read_string(reader, child_len)?);
            }
            // Unused, but read as integers so a malformed value fails the probe.
            ID_TRACK_NUMBER | ID_FLAG_DEFAULT | ID_FLAG_FORCED => {
                read_uint(reader, child_len)?;
            }
            ID_VIDEO => {
                let v_end = element_end(reader, child_len, "Video element length overflow")?;
                while reader.stream_position()? < v_end {
                    let vid_child = read_element_id(reader)?;
                    let vid_len = read_vint_length(reader)?;
                    match vid_child {
                        ID_PIXEL_WIDTH => {
                            pixel_width = Some(read_uint(reader, vid_len)? as u32);
                        }
                        ID_PIXEL_HEIGHT => {
                            pixel_height = Some(read_uint(reader, vid_len)? as u32);
                        }
                        _ => {
                            seek_forward(reader, vid_len)?;
                        }
                    }
                }
            }
            ID_AUDIO => {
                let mut explicit_channels: Option<u32> = None;
                let a_end = element_end(reader, child_len, "Audio element length overflow")?;
                while reader.stream_position()? < a_end {
                    let aud_child = read_element_id(reader)?;
                    let aud_len = read_vint_length(reader)?;
                    match aud_child {
                        ID_CHANNELS => {
                            explicit_channels = Some(read_uint(reader, aud_len)? as u32);
                        }
                        _ => {
                            seek_forward(reader, aud_len)?;
                        }
                    }
                }
                // Matroska spec RFC 9559 §21.1: Channels defaults to 1 when omitted.
                channels = Some(explicit_channels.unwrap_or(1));
            }
            _ => {
                seek_forward(reader, child_len)?;
            }
        }
    }

    let track_type = match track_type_num {
        Some(1) => "video",
        Some(2) => "audio",
        Some(17) => "subtitles",
        _ => "unknown",
    }
    .to_string();

    let pixel_dimensions = match (pixel_width, pixel_height) {
        (Some(w), Some(h)) => Some(format!("{w}x{h}")),
        _ => None,
    };

    let resolved_language = language.or_else(|| {
        language_ietf.map(|ietf| {
            let base = ietf.split('-').next().unwrap_or(&ietf);
            lang_mkv_tag(base).to_string()
        })
    });

    let properties = TrackProperties {
        codec_id: codec_id.clone(),
        language: resolved_language,
        track_name,
        audio_channels: channels,
        pixel_dimensions,
    };

    Ok(MkvTrack {
        id: None,
        track_type,
        codec: codec_id,
        properties,
    })
}

fn parse_attached_file<R: Read + Seek>(reader: &mut R, len: u64) -> io::Result<MkvAttachment> {
    let end = element_end(reader, len, "Attached file length overflow")?;

    let mut file_name: Option<String> = None;
    let mut content_type: Option<String> = None;
    let mut size: Option<u64> = None;

    while reader.stream_position()? < end {
        let child_id = read_element_id(reader)?;
        let child_len = read_vint_length(reader)?;

        match child_id {
            ID_FILE_NAME => {
                file_name = Some(read_string(reader, child_len)?);
            }
            ID_FILE_MIME => {
                content_type = Some(read_string(reader, child_len)?);
            }
            ID_FILE_DATA => {
                size = Some(child_len);
                seek_forward(reader, child_len)?;
            }
            _ => {
                seek_forward(reader, child_len)?;
            }
        }
    }

    Ok(MkvAttachment {
        id: None,
        file_name,
        content_type,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(buf: &[u8]) -> MkvTrack {
        parse_track_entry(&mut Cursor::new(buf), buf.len() as u64).unwrap()
    }

    fn make_track_buf(lang: Option<&str>, lang_ietf: Option<&str>) -> Vec<u8> {
        // TrackType = 17 (subtitles)
        let mut buf = vec![0x83, 0x81, 17];
        if let Some(l) = lang {
            buf.extend_from_slice(&[0x22, 0xB5, 0x9C]);
            buf.push(0x80 | (l.len() as u8));
            buf.extend_from_slice(l.as_bytes());
        }
        if let Some(li) = lang_ietf {
            buf.extend_from_slice(&[0x22, 0xB5, 0x9D]);
            buf.push(0x80 | (li.len() as u8));
            buf.extend_from_slice(li.as_bytes());
        }
        buf
    }

    #[test]
    fn test_track_language_priority() {
        // (Language, LanguageIETF, expected): ISO 639-2 `Language` wins; otherwise the
        // BCP 47 primary subtag is mapped through `lang_mkv_tag`.
        let cases = [
            (Some("jpn"), Some("en-US"), Some("jpn")),
            (None, Some("id-ID"), Some("ind")),
            (None, Some("en-US"), Some("eng")),
            (None, Some("ja"), Some("jpn")),
            (Some("ind"), None, Some("ind")),
            (None, None, None),
        ];
        for (lang, lang_ietf, expected) in cases {
            let track = parse(&make_track_buf(lang, lang_ietf));
            assert_eq!(
                track.properties.language.as_deref(),
                expected,
                "Language={lang:?} LanguageIETF={lang_ietf:?}"
            );
        }
    }

    fn make_audio_track_buf(audio_block: &[u8]) -> Vec<u8> {
        // TrackType = 2 (audio)
        let mut buf = vec![0x83, 0x81, 0x02];
        buf.extend_from_slice(audio_block);
        buf
    }

    #[test]
    fn test_audio_channels_spec_default() {
        // Audio (0xE1) with length 0: Channels omitted, defaults to 1 per RFC 9559 §21.1.
        let mono = parse(&make_audio_track_buf(&[0xE1, 0x80]));
        assert_eq!(mono.properties.audio_channels, Some(1));

        // Audio (0xE1) len 3 containing Channels (0x9F) len 1 = 2.
        let stereo = parse(&make_audio_track_buf(&[0xE1, 0x83, 0x9F, 0x81, 0x02]));
        assert_eq!(stereo.properties.audio_channels, Some(2));

        // No Audio element at all.
        let subtitle = parse(&make_track_buf(None, None));
        assert_eq!(subtitle.properties.audio_channels, None);
    }
}
