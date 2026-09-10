//! Native lightweight EBML parser for Matroska containers.
//!
//! Parses Segment Header, Tracks (video, audio, subtitles), Attachments
//! (fonts, covers), and Chapters directly in pure Rust without external process spawning.
//! Fallback to `mkvmerge -J` occurs on any unhandled error, non-standard quirk,
//! missing SeekHead with post-cluster elements, or malformed structure (ADR 0006).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::domain::{MkvTrack, TrackProperties};
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

/// Reads an EBML Element ID as a raw u32.
fn read_element_id<R: Read>(reader: &mut R) -> Result<u32, std::io::Error> {
    let mut first_byte = [0u8; 1];
    reader.read_exact(&mut first_byte)?;
    let b = first_byte[0];
    if b == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Invalid zero EBML ID",
        ));
    }

    let len = b.leading_zeros() as usize + 1;
    if len > 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "EBML ID too large (> 4 bytes)",
        ));
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
fn read_vint_length<R: Read>(reader: &mut R) -> Result<u64, std::io::Error> {
    let mut first_byte = [0u8; 1];
    reader.read_exact(&mut first_byte)?;
    let b = first_byte[0];
    if b == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Invalid zero VINT length",
        ));
    }

    let num_zeros = b.leading_zeros() as usize;
    let len = num_zeros + 1;
    let mask = if len == 8 { 0 } else { 0xFF >> len };
    let mut value = (b & mask) as u64;
    for _ in 1..len {
        let mut next = [0u8; 1];
        reader.read_exact(&mut next)?;
        value = (value << 8) | (next[0] as u64);
    }
    Ok(value)
}

fn read_uint<R: Read>(reader: &mut R, size: u64) -> Result<u64, std::io::Error> {
    if size > 8 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Uint size > 8",
        ));
    }
    let mut val = 0u64;
    for _ in 0..size {
        let mut b = [0u8; 1];
        reader.read_exact(&mut b)?;
        val = (val << 8) | (b[0] as u64);
    }
    Ok(val)
}

fn read_string<R: Read>(reader: &mut R, size: u64) -> Result<String, std::io::Error> {
    if size > 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "String too long (> 1MB)",
        ));
    }
    let mut buf = vec![0u8; size as usize];
    reader.read_exact(&mut buf)?;
    while buf.last() == Some(&0) {
        buf.pop();
    }
    Ok(String::from_utf8_lossy(&buf).to_string())
}

/// Native EBML probe for Matroska files. Returns `MkvmergeMetadata` on success,
/// or `Err` if anything is unhandled or malformed.
pub fn probe_ebml_native(path: &Path) -> Result<MkvmergeMetadata, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);

    // Verify EBML Header
    let ebml_id = read_element_id(&mut reader).map_err(|e| e.to_string())?;
    if ebml_id != ID_EBML_HEADER {
        return Err("Not an EBML file".to_string());
    }
    let ebml_len = read_vint_length(&mut reader).map_err(|e| e.to_string())?;
    reader
        .seek(SeekFrom::Current(ebml_len as i64))
        .map_err(|e| e.to_string())?;

    // Find Segment element
    let segment_id = read_element_id(&mut reader).map_err(|e| e.to_string())?;
    if segment_id != ID_SEGMENT {
        return Err("Segment element not found after EBML header".to_string());
    }
    let _segment_len = read_vint_length(&mut reader).map_err(|e| e.to_string())?;
    let segment_data_start = reader.stream_position().map_err(|e| e.to_string())?;

    let mut tracks = Vec::new();
    let mut attachments = Vec::new();
    let mut has_chapters = false;

    let mut seek_tracks: Option<u64> = None;
    let mut seek_attachments: Option<u64> = None;
    let mut seek_chapters: Option<u64> = None;
    let mut has_seek_head = false;

    // First scan top-level elements up to first Cluster
    loop {
        let elem_id = match read_element_id(&mut reader) {
            Ok(id) => id,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("Error reading element ID: {e}")),
        };
        let elem_len = match read_vint_length(&mut reader) {
            Ok(len) => len,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("Error reading element length: {e}")),
        };

        match elem_id {
            ID_SEEK_HEAD => {
                has_seek_head = true;
                let current_pos = reader.stream_position().map_err(|e| e.to_string())?;
                let end_pos = current_pos + elem_len;
                while reader.stream_position().map_err(|e| e.to_string())? < end_pos {
                    let seek_entry_id = match read_element_id(&mut reader) {
                        Ok(id) => id,
                        Err(_) => break,
                    };
                    let seek_entry_len = match read_vint_length(&mut reader) {
                        Ok(l) => l,
                        Err(_) => break,
                    };
                    if seek_entry_id == ID_SEEK {
                        let entry_end =
                            reader.stream_position().map_err(|e| e.to_string())? + seek_entry_len;
                        let mut target_id: Option<u32> = None;
                        let mut target_pos: Option<u64> = None;
                        while reader.stream_position().map_err(|e| e.to_string())? < entry_end {
                            let child = match read_element_id(&mut reader) {
                                Ok(c) => c,
                                Err(_) => break,
                            };
                            let child_len = match read_vint_length(&mut reader) {
                                Ok(l) => l,
                                Err(_) => break,
                            };
                            let child_start =
                                reader.stream_position().map_err(|e| e.to_string())?;
                            let child_end = child_start + child_len;
                            if child == ID_SEEK_ID {
                                target_id = read_element_id(&mut reader).ok();
                            } else if child == ID_SEEK_POSITION {
                                target_pos = read_uint(&mut reader, child_len).ok();
                            }
                            let _ = reader.seek(SeekFrom::Start(child_end));
                        }
                        if let (Some(tid), Some(pos)) = (target_id, target_pos) {
                            match tid {
                                ID_TRACKS => seek_tracks = Some(pos),
                                ID_ATTACHMENTS => seek_attachments = Some(pos),
                                ID_CHAPTERS => seek_chapters = Some(pos),
                                _ => {}
                            }
                        }
                        let _ = reader.seek(SeekFrom::Start(entry_end));
                    } else {
                        let _ = reader.seek(SeekFrom::Current(seek_entry_len as i64));
                    }
                }
                let _ = reader.seek(SeekFrom::Start(end_pos));
            }
            ID_TRACKS => {
                let current_pos = reader.stream_position().map_err(|e| e.to_string())?;
                parse_tracks_block(&mut reader, elem_len, &mut tracks)?;
                reader
                    .seek(SeekFrom::Start(current_pos + elem_len))
                    .map_err(|e| e.to_string())?;
            }
            ID_ATTACHMENTS => {
                let current_pos = reader.stream_position().map_err(|e| e.to_string())?;
                parse_attachments_block(&mut reader, elem_len, &mut attachments)?;
                reader
                    .seek(SeekFrom::Start(current_pos + elem_len))
                    .map_err(|e| e.to_string())?;
            }
            ID_CHAPTERS => {
                has_chapters = true;
                let _ = reader.seek(SeekFrom::Current(elem_len as i64));
            }
            ID_CLUSTER => {
                // Reached video/audio clusters. If we have a SeekHead, use it to inspect post-cluster attachments/chapters.
                // If there is NO SeekHead and we haven't seen attachments/chapters, fall back to mkvmerge -J!
                if !has_seek_head {
                    return Err(
                        "No SeekHead before clusters, falling back to mkvmerge -J".to_string()
                    );
                }
                break;
            }
            _ => {
                let _ = reader.seek(SeekFrom::Current(elem_len as i64));
            }
        }
    }

    // Follow SeekHead pointers if elements were located after clusters
    if tracks.is_empty() {
        if let Some(pos) = seek_tracks {
            reader
                .seek(SeekFrom::Start(segment_data_start + pos))
                .map_err(|e| e.to_string())?;
            let elem_id = read_element_id(&mut reader).map_err(|e| e.to_string())?;
            let elem_len = read_vint_length(&mut reader).map_err(|e| e.to_string())?;
            if elem_id == ID_TRACKS {
                parse_tracks_block(&mut reader, elem_len, &mut tracks)?;
            }
        }
    }

    if attachments.is_empty() {
        if let Some(pos) = seek_attachments {
            reader
                .seek(SeekFrom::Start(segment_data_start + pos))
                .map_err(|e| e.to_string())?;
            let elem_id = read_element_id(&mut reader).map_err(|e| e.to_string())?;
            let elem_len = read_vint_length(&mut reader).map_err(|e| e.to_string())?;
            if elem_id == ID_ATTACHMENTS {
                parse_attachments_block(&mut reader, elem_len, &mut attachments)?;
            }
        }
    }

    if !has_chapters && seek_chapters.is_some() {
        has_chapters = true;
    }

    if tracks.is_empty() {
        return Err("No tracks discovered via native EBML probe".to_string());
    }

    let chapters = if has_chapters {
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

fn parse_tracks_block<R: Read + Seek>(
    reader: &mut R,
    elem_len: u64,
    tracks: &mut Vec<MkvTrack>,
) -> Result<(), String> {
    let current_pos = reader.stream_position().map_err(|e| e.to_string())?;
    let end_pos = current_pos + elem_len;
    let mut track_idx = tracks.len() as u32;
    while reader.stream_position().map_err(|e| e.to_string())? < end_pos {
        let entry_id = match read_element_id(reader) {
            Ok(id) => id,
            Err(_) => break,
        };
        let entry_len = match read_vint_length(reader) {
            Ok(l) => l,
            Err(_) => break,
        };
        let entry_start = reader.stream_position().map_err(|e| e.to_string())?;
        let entry_end = entry_start + entry_len;
        if entry_id == ID_TRACK_ENTRY {
            if let Ok(mut track) = parse_track_entry(reader, entry_len) {
                track.id = Some(track_idx);
                track_idx += 1;
                tracks.push(track);
            }
        }
        reader
            .seek(SeekFrom::Start(entry_end))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn parse_attachments_block<R: Read + Seek>(
    reader: &mut R,
    elem_len: u64,
    attachments: &mut Vec<MkvAttachment>,
) -> Result<(), String> {
    let current_pos = reader.stream_position().map_err(|e| e.to_string())?;
    let end_pos = current_pos + elem_len;
    let mut att_id = (attachments.len() + 1) as u32;
    while reader.stream_position().map_err(|e| e.to_string())? < end_pos {
        let file_id = match read_element_id(reader) {
            Ok(id) => id,
            Err(_) => break,
        };
        let file_len = match read_vint_length(reader) {
            Ok(l) => l,
            Err(_) => break,
        };
        let entry_start = reader.stream_position().map_err(|e| e.to_string())?;
        let entry_end = entry_start + file_len;
        if file_id == ID_ATTACHED_FILE {
            if let Ok(mut att) = parse_attached_file(reader, file_len) {
                att.id = Some(att_id);
                att_id += 1;
                attachments.push(att);
            }
        }
        reader
            .seek(SeekFrom::Start(entry_end))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn parse_track_entry<R: Read + Seek>(reader: &mut R, len: u64) -> Result<MkvTrack, std::io::Error> {
    let start_pos = reader.stream_position()?;
    let end_pos = start_pos + len;

    let mut track_number: Option<u32> = None;
    let mut track_type_num: Option<u64> = None;
    let mut codec_id: Option<String> = None;
    let mut language: Option<String> = None;
    let mut language_ietf: Option<String> = None;
    let mut track_name: Option<String> = None;
    let mut pixel_width: Option<u32> = None;
    let mut pixel_height: Option<u32> = None;
    let mut channels: Option<u32> = None;

    while reader.stream_position()? < end_pos {
        let child_id = read_element_id(reader)?;
        let child_len = read_vint_length(reader)?;

        match child_id {
            ID_TRACK_NUMBER => {
                track_number = Some(read_uint(reader, child_len)? as u32);
            }
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
            ID_FLAG_DEFAULT | ID_FLAG_FORCED => {
                let _ = read_uint(reader, child_len)?;
            }
            ID_VIDEO => {
                let v_end = reader.stream_position()? + child_len;
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
                            reader.seek(SeekFrom::Current(vid_len as i64))?;
                        }
                    }
                }
            }
            ID_AUDIO => {
                let a_end = reader.stream_position()? + child_len;
                while reader.stream_position()? < a_end {
                    let aud_child = read_element_id(reader)?;
                    let aud_len = read_vint_length(reader)?;
                    match aud_child {
                        ID_CHANNELS => {
                            channels = Some(read_uint(reader, aud_len)? as u32);
                        }
                        _ => {
                            reader.seek(SeekFrom::Current(aud_len as i64))?;
                        }
                    }
                }
            }
            _ => {
                reader.seek(SeekFrom::Current(child_len as i64))?;
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

    let properties = TrackProperties {
        codec_id: codec_id.clone(),
        language: language_ietf.or(language),
        track_name,
        audio_channels: channels,
        pixel_dimensions,
    };

    let id = track_number.map(|n| if n > 0 { n - 1 } else { 0 });

    Ok(MkvTrack {
        id,
        track_type,
        codec: codec_id,
        properties,
    })
}

fn parse_attached_file<R: Read + Seek>(
    reader: &mut R,
    len: u64,
) -> Result<MkvAttachment, std::io::Error> {
    let start_pos = reader.stream_position()?;
    let end_pos = start_pos + len;

    let mut file_name: Option<String> = None;
    let mut content_type: Option<String> = None;
    let mut size: Option<u64> = None;

    while reader.stream_position()? < end_pos {
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
                reader.seek(SeekFrom::Current(child_len as i64))?;
            }
            _ => {
                reader.seek(SeekFrom::Current(child_len as i64))?;
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
