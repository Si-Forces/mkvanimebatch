//! Native EBML parser for Matroska containers.
//!
//! Reads Tracks (video, audio, subtitles), Attachments (fonts, covers), and whether Chapters
//! exist, without running a Media Tool. A truncated tail or a malformed SeekHead entry ends the
//! scan without failing. Any error (not an EBML/Matroska file, a Cluster before any SeekHead, a
//! malformed Tracks or Attachments element, no track found, I/O failure) makes the caller fall
//! back to `mkvmerge -J` (ADR-0006).

use std::fs::File;
use std::io::{self, BufReader, Read, Seek};
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

/// Seeks with `BufReader::seek_relative`, which keeps the buffer when the target lies inside it,
/// and tracks the stream position itself, so reading the position never issues an `lseek`.
struct EbmlReader<R> {
    inner: BufReader<R>,
    pos: u64,
}

impl<R: Read + Seek> EbmlReader<R> {
    fn new(reader: R) -> Self {
        Self {
            inner: BufReader::new(reader),
            pos: 0,
        }
    }

    fn pos(&self) -> u64 {
        self.pos
    }

    fn skip(&mut self, len: u64) -> io::Result<()> {
        let target = self.element_end(len, "EBML element end overflow")?;
        self.seek_to(target)
    }

    fn seek_to(&mut self, target: u64) -> io::Result<()> {
        let target_i64 =
            i64::try_from(target).map_err(|_| invalid_data("EBML seek target exceeds i64::MAX"))?;
        let pos_i64 = i64::try_from(self.pos)
            .map_err(|_| invalid_data("EBML current pos exceeds i64::MAX"))?;
        let offset = target_i64 - pos_i64;
        self.inner.seek_relative(offset)?;
        self.pos = target;
        Ok(())
    }

    fn element_end(&self, len: u64, overflow_msg: &'static str) -> io::Result<u64> {
        self.pos
            .checked_add(len)
            .ok_or_else(|| invalid_data(overflow_msg))
    }
}

impl<R: Read> Read for EbmlReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.pos = self
            .pos
            .checked_add(n as u64)
            .ok_or_else(|| invalid_data("EBML reader position overflow"))?;
        Ok(n)
    }
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
    if len > 1 {
        let mut rem = [0u8; 3];
        reader.read_exact(&mut rem[..len - 1])?;
        for &next in &rem[..len - 1] {
            id = (id << 8) | (next as u32);
        }
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
    if len > 1 {
        let mut rem = [0u8; 7];
        reader.read_exact(&mut rem[..len - 1])?;
        for &next in &rem[..len - 1] {
            value = (value << 8) | (next as u64);
        }
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
    let mut buf = [0u8; 8];
    let len = size as usize;
    reader.read_exact(&mut buf[..len])?;
    let mut val = 0u64;
    for &b in &buf[..len] {
        val = (val << 8) | (b as u64);
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
    let mut reader = EbmlReader::new(File::open(path)?);

    if read_element_id(&mut reader)? != ID_EBML_HEADER {
        return Err(invalid_data("Not an EBML file"));
    }
    let ebml_len = read_vint_length(&mut reader)?;
    reader.skip(ebml_len)?;

    if read_element_id(&mut reader)? != ID_SEGMENT {
        return Err(invalid_data("Segment element not found after EBML header"));
    }
    let _segment_len = read_vint_length(&mut reader)?;
    let segment_data_start = reader.pos();

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
                let end = reader.element_end(elem_len, "SeekHead element length overflow")?;
                read_seek_head(&mut reader, end, &mut seek_targets)?;
                reader.seek_to(end)?;
            }
            ID_TRACKS => {
                let end = reader.element_end(elem_len, "Tracks block length overflow")?;
                parse_tracks_block(&mut reader, elem_len, &mut tracks)?;
                reader.seek_to(end)?;
            }
            ID_ATTACHMENTS => {
                let end = reader.element_end(elem_len, "Attachments block length overflow")?;
                parse_attachments_block(&mut reader, elem_len, &mut attachments)?;
                reader.seek_to(end)?;
            }
            ID_CHAPTERS => {
                has_chapters = true;
                if reader.skip(elem_len).is_err() {
                    break;
                }
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
                if reader.skip(elem_len).is_err() {
                    break;
                }
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
    reader: &mut EbmlReader<R>,
    end: u64,
    targets: &mut SeekTargets,
) -> io::Result<()> {
    while reader.pos() < end {
        let Ok(entry_id) = read_element_id(reader) else {
            break;
        };
        let Ok(entry_len) = read_vint_length(reader) else {
            break;
        };
        if entry_id != ID_SEEK {
            if reader.skip(entry_len).is_err() {
                break;
            }
            continue;
        }

        let Ok(entry_end) = reader.element_end(entry_len, "Seek entry length overflow") else {
            break;
        };
        let mut target_id: Option<u32> = None;
        let mut target_pos: Option<u64> = None;
        while reader.pos() < entry_end {
            let Ok(child) = read_element_id(reader) else {
                break;
            };
            let Ok(child_len) = read_vint_length(reader) else {
                break;
            };
            let Ok(child_end) = reader.element_end(child_len, "Seek child length overflow") else {
                break;
            };
            match child {
                ID_SEEK_ID => target_id = read_element_id(reader).ok(),
                ID_SEEK_POSITION => target_pos = read_uint(reader, child_len).ok(),
                _ => {}
            }
            if reader.seek_to(child_end).is_err() {
                break;
            }
        }
        if let (Some(id), Some(pos)) = (target_id, target_pos) {
            match id {
                ID_TRACKS => targets.tracks = Some(pos),
                ID_ATTACHMENTS => targets.attachments = Some(pos),
                ID_CHAPTERS => targets.chapters = true,
                _ => {}
            }
        }
        if reader.seek_to(entry_end).is_err() {
            break;
        }
    }
    Ok(())
}

/// Seeks to the element `pos` bytes into the Segment payload and reads its header.
fn element_at<R: Read + Seek>(
    reader: &mut EbmlReader<R>,
    segment_data_start: u64,
    pos: u64,
    overflow_msg: &'static str,
) -> io::Result<(u32, u64)> {
    let target = segment_data_start
        .checked_add(pos)
        .ok_or_else(|| invalid_data(overflow_msg))?;
    reader.seek_to(target)?;
    Ok((read_element_id(reader)?, read_vint_length(reader)?))
}

/// Appends each TrackEntry, numbered sequentially from `tracks.len()` as mkvmerge does.
fn parse_tracks_block<R: Read + Seek>(
    reader: &mut EbmlReader<R>,
    len: u64,
    tracks: &mut Vec<MkvTrack>,
) -> io::Result<()> {
    let end = reader.element_end(len, "Tracks block length overflow")?;
    let mut track_idx = tracks.len() as u32;
    while reader.pos() < end {
        let entry_id = read_element_id(reader)?;
        let entry_len = read_vint_length(reader)?;
        let entry_end = reader.element_end(entry_len, "Track entry length overflow")?;
        if entry_id == ID_TRACK_ENTRY {
            let mut track = parse_track_entry(reader, entry_len)?;
            track.id = Some(track_idx);
            track_idx += 1;
            tracks.push(track);
        }
        reader.seek_to(entry_end)?;
    }
    Ok(())
}

/// Appends each AttachedFile, numbered sequentially from 1 as mkvmerge does.
fn parse_attachments_block<R: Read + Seek>(
    reader: &mut EbmlReader<R>,
    len: u64,
    attachments: &mut Vec<MkvAttachment>,
) -> io::Result<()> {
    let end = reader.element_end(len, "Attachments block length overflow")?;
    let mut att_id = (attachments.len() + 1) as u32;
    while reader.pos() < end {
        let file_id = read_element_id(reader)?;
        let file_len = read_vint_length(reader)?;
        let entry_end = reader.element_end(file_len, "Attached file length overflow")?;
        if file_id == ID_ATTACHED_FILE {
            let mut att = parse_attached_file(reader, file_len)?;
            att.id = Some(att_id);
            att_id += 1;
            attachments.push(att);
        }
        reader.seek_to(entry_end)?;
    }
    Ok(())
}

/// Parses one TrackEntry. The returned track has no `id`; the caller numbers it.
fn parse_track_entry<R: Read + Seek>(reader: &mut EbmlReader<R>, len: u64) -> io::Result<MkvTrack> {
    let end = reader.element_end(len, "Track entry length overflow")?;

    let mut track_type_num: Option<u64> = None;
    let mut codec_id: Option<String> = None;
    let mut language: Option<String> = None;
    let mut language_ietf: Option<String> = None;
    let mut track_name: Option<String> = None;
    let mut pixel_width: Option<u32> = None;
    let mut pixel_height: Option<u32> = None;
    let mut channels: Option<u32> = None;

    while reader.pos() < end {
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
                let v_end = reader.element_end(child_len, "Video element length overflow")?;
                while reader.pos() < v_end {
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
                            reader.skip(vid_len)?;
                        }
                    }
                }
            }
            ID_AUDIO => {
                let mut explicit_channels: Option<u32> = None;
                let a_end = reader.element_end(child_len, "Audio element length overflow")?;
                while reader.pos() < a_end {
                    let aud_child = read_element_id(reader)?;
                    let aud_len = read_vint_length(reader)?;
                    match aud_child {
                        ID_CHANNELS => {
                            explicit_channels = Some(read_uint(reader, aud_len)? as u32);
                        }
                        _ => {
                            reader.skip(aud_len)?;
                        }
                    }
                }
                // Matroska spec RFC 9559 §21.1: Channels defaults to 1 when omitted.
                channels = Some(explicit_channels.unwrap_or(1));
            }
            _ => {
                reader.skip(child_len)?;
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

fn parse_attached_file<R: Read + Seek>(
    reader: &mut EbmlReader<R>,
    len: u64,
) -> io::Result<MkvAttachment> {
    let end = reader.element_end(len, "Attached file length overflow")?;

    let mut file_name: Option<String> = None;
    let mut content_type: Option<String> = None;
    let mut size: Option<u64> = None;

    while reader.pos() < end {
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
                reader.skip(child_len)?;
            }
            _ => {
                reader.skip(child_len)?;
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
        let mut reader = EbmlReader::new(Cursor::new(buf));
        parse_track_entry(&mut reader, buf.len() as u64).unwrap()
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

    struct TrackingReader<R> {
        inner: R,
        reads: usize,
        seeks: usize,
    }

    impl<R: Read> Read for TrackingReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            self.inner.read(buf)
        }
    }

    impl<R: Seek> Seek for TrackingReader<R> {
        fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
            self.seeks += 1;
            self.inner.seek(pos)
        }
    }

    #[test]
    fn test_ebml_reader_retains_buffer_on_intra_buffer_skips() {
        let data = vec![0x42u8; 1024];
        let tracker = TrackingReader {
            inner: Cursor::new(data),
            reads: 0,
            seeks: 0,
        };
        let mut reader = EbmlReader::new(tracker);

        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(reader.pos(), 4);
        assert_eq!(reader.inner.get_ref().reads, 1);
        assert_eq!(reader.inner.get_ref().seeks, 0);

        // Relative skip forward by 10 bytes within the 8KB buffer
        reader.skip(10).unwrap();
        assert_eq!(reader.pos(), 14);
        // Intra-buffer skip must NOT issue an underlying read or seek
        assert_eq!(reader.inner.get_ref().reads, 1);
        assert_eq!(reader.inner.get_ref().seeks, 0);

        // Read next 4 bytes: served directly from memory buffer
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(reader.pos(), 18);
        assert_eq!(reader.inner.get_ref().reads, 1);
        assert_eq!(reader.inner.get_ref().seeks, 0);

        // seek_to forward to 50
        reader.seek_to(50).unwrap();
        assert_eq!(reader.pos(), 50);
        assert_eq!(reader.inner.get_ref().reads, 1);
        assert_eq!(reader.inner.get_ref().seeks, 0);
    }

    fn make_synthetic_mkv_multitrack() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&[0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42, 0x82, 0x84, 0x6D]);
        data.extend_from_slice(&[
            0x18, 0x53, 0x80, 0x67, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        ]);
        // Top-level SeekHead element before clusters
        data.extend_from_slice(&[0x11, 0x4D, 0x9B, 0x74, 0x80]);

        let mut tracks_payload = Vec::new();
        for t in 0..6 {
            let mut track_entry = Vec::new();
            track_entry.extend_from_slice(&[0xD7, 0x81, (t + 1) as u8]);
            track_entry.extend_from_slice(&[0x73, 0xC5, 0x84, 1, 2, 3, 4]);
            let tt = if t == 0 {
                1
            } else if t < 4 {
                2
            } else {
                17
            };
            track_entry.extend_from_slice(&[0x83, 0x81, tt]);
            track_entry.extend_from_slice(&[0x88, 0x81, 1]);
            track_entry.extend_from_slice(&[0x55, 0xAA, 0x81, 0]);
            track_entry.extend_from_slice(&[0x9C, 0x81, 0]);
            let codec = if t == 0 {
                "V_MPEGH/HEVC"
            } else if t < 4 {
                "A_OPUS"
            } else {
                "S_TEXT/ASS"
            };
            track_entry.extend_from_slice(&[0x86, 0x80 | (codec.len() as u8)]);
            track_entry.extend_from_slice(codec.as_bytes());
            track_entry.extend_from_slice(&[0x63, 0xA2, 0x80 | 20]);
            track_entry.extend_from_slice(&[0u8; 20]);
            track_entry.extend_from_slice(&[0x22, 0xB5, 0x9C, 0x83, b'j', b'p', b'n']);
            let name = format!("Track {:02}", t);
            track_entry.extend_from_slice(&[0x53, 0x6E, 0x80 | (name.len() as u8)]);
            track_entry.extend_from_slice(name.as_bytes());

            if t == 0 {
                let mut vid = Vec::new();
                vid.extend_from_slice(&[0xB0, 0x82, 0x07, 0x80]);
                vid.extend_from_slice(&[0xBA, 0x82, 0x04, 0x38]);
                vid.extend_from_slice(&[0x54, 0xB0, 0x82, 0x07, 0x80]);
                track_entry.extend_from_slice(&[0xE0, 0x80 | (vid.len() as u8)]);
                track_entry.extend_from_slice(&vid);
            } else if t < 4 {
                let mut aud = Vec::new();
                aud.extend_from_slice(&[0xB5, 0x84, 0x47, 0x3B, 0x80, 0x00]);
                aud.extend_from_slice(&[0x9F, 0x81, 2]);
                track_entry.extend_from_slice(&[0xE1, 0x80 | (aud.len() as u8)]);
                track_entry.extend_from_slice(&aud);
            }

            tracks_payload.push(0xAE);
            let len = track_entry.len();
            if len < 127 {
                tracks_payload.push(0x80 | (len as u8));
            } else {
                tracks_payload.push(0x40 | ((len >> 8) as u8));
                tracks_payload.push((len & 0xFF) as u8);
            }
            tracks_payload.extend_from_slice(&track_entry);
        }

        data.extend_from_slice(&[0x16, 0x54, 0xAE, 0x6B]);
        let tlen = tracks_payload.len();
        data.push(0x40 | ((tlen >> 8) as u8));
        data.push((tlen & 0xFF) as u8);
        data.extend_from_slice(&tracks_payload);

        data.extend_from_slice(&[0x1F, 0x43, 0xB6, 0x75, 0x84, 0, 0, 0, 0]);
        data
    }

    #[test]
    fn test_synthetic_multitrack_mkv_native_probe_and_buffer_retention() {
        let mkv_bytes = make_synthetic_mkv_multitrack();

        // 1. Verify probe_ebml_native parses the synthetic file correctly from disk
        use std::io::Write;
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        temp.write_all(&mkv_bytes).unwrap();
        let meta = probe_ebml_native(temp.path()).unwrap();
        assert_eq!(meta.tracks.len(), 6);

        // 2. Parsing the same bytes keeps the buffer: one fill and no seeks
        let tracker = TrackingReader {
            inner: Cursor::new(mkv_bytes),
            reads: 0,
            seeks: 0,
        };
        let mut reader = EbmlReader::new(tracker);

        assert_eq!(read_element_id(&mut reader).unwrap(), ID_EBML_HEADER);
        let ebml_len = read_vint_length(&mut reader).unwrap();
        reader.skip(ebml_len).unwrap();

        assert_eq!(read_element_id(&mut reader).unwrap(), ID_SEGMENT);
        let _ = read_vint_length(&mut reader).unwrap();

        let mut tracks = Vec::new();
        loop {
            let elem_id = read_element_id(&mut reader).unwrap();
            let elem_len = read_vint_length(&mut reader).unwrap();
            match elem_id {
                ID_TRACKS => {
                    let end = reader.element_end(elem_len, "overflow").unwrap();
                    parse_tracks_block(&mut reader, elem_len, &mut tracks).unwrap();
                    reader.seek_to(end).unwrap();
                }
                ID_CLUSTER => break,
                _ => {
                    if reader.skip(elem_len).is_err() {
                        break;
                    }
                }
            }
        }

        assert_eq!(tracks.len(), 6);
        let raw = reader.inner.into_inner();
        assert_eq!(raw.reads, 1, "Expected single read buffer fill (<8 KiB)");
        assert_eq!(
            raw.seeks, 0,
            "Expected 0 seek calls during intra-buffer parse"
        );
    }

    #[test]
    fn test_ebml_reader_multi_buffer_reads() {
        // Create 12 KiB payload: crosses 8 KiB BufReader boundary
        let data = vec![0x7Au8; 12 * 1024];
        let tracker = TrackingReader {
            inner: Cursor::new(data),
            reads: 0,
            seeks: 0,
        };
        let mut reader = EbmlReader::new(tracker);

        let mut buf = [0u8; 100];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(reader.inner.get_ref().reads, 1);

        // Skip to 9 KiB (across 8 KiB boundary)
        reader.skip(9 * 1024).unwrap();
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(reader.inner.get_ref().reads, 2);
    }
}
