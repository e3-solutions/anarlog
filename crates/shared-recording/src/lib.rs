#![forbid(unsafe_code)]

mod alignment;
pub use alignment::align_clock_hints;

use std::io::Cursor;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SAMPLE_RATE: u32 = 16_000;
pub const MAX_FRAMES: u64 = SAMPLE_RATE as u64 * 60 * 60 * 4;
pub const MAX_SOURCES: usize = 32;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrameRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone)]
pub struct Source {
    pub capture_id: String,
    pub sha256: String,
    pub offset_frames: i64,
    pub rank: u32,
    pub samples: Vec<i16>,
    pub gaps: Vec<FrameRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Selection {
    pub start_frame: u64,
    pub end_frame: u64,
    pub capture_id: Option<String>,
    pub source_sha256: Option<String>,
    pub source_start_frame: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    pub sample_rate: u32,
    pub duration_frames: u64,
    pub selections: Vec<Selection>,
}

#[derive(Debug)]
pub struct CanonicalRecording {
    pub wav: Vec<u8>,
    pub sha256: String,
    pub revision: String,
    pub manifest: Manifest,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("recording exceeds supported limits")]
    Limit,
    #[error("expected nonempty 16 kHz mono signed PCM16 WAV")]
    Format,
    #[error("invalid source identity, timing or declared gaps")]
    InvalidSource,
    #[error(transparent)]
    Wav(#[from] hound::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn decode_source(
    capture_id: String,
    rank: u32,
    offset_frames: i64,
    wav: &[u8],
    gaps: Vec<FrameRange>,
) -> Result<Source, Error> {
    if wav.len() as u64 > MAX_FRAMES * 2 + 4096 {
        return Err(Error::Limit);
    }
    let mut reader = hound::WavReader::new(Cursor::new(wav))?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != SAMPLE_RATE
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
        || reader.len() == 0
    {
        return Err(Error::Format);
    }
    if reader.len() as u64 > MAX_FRAMES {
        return Err(Error::Limit);
    }
    let source = Source {
        capture_id,
        sha256: sha256(wav),
        offset_frames,
        rank,
        samples: reader.samples::<i16>().collect::<Result<Vec<_>, _>>()?,
        gaps,
    };
    validate_source(&source)?;
    Ok(source)
}

fn validate_source(source: &Source) -> Result<(), Error> {
    if source.capture_id.is_empty()
        || source.capture_id.len() > 256
        || source.samples.is_empty()
        || source.samples.len() as u64 > MAX_FRAMES
        || source.offset_frames.unsigned_abs() > MAX_FRAMES
        || source.sha256.len() != 64
        || !source.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || source.gaps.len() > 4096
        || source
            .gaps
            .iter()
            .any(|gap| gap.start >= gap.end || gap.end > source.samples.len() as u64)
    {
        return Err(Error::InvalidSource);
    }
    Ok(())
}

pub fn render(duration_frames: u64, sources: &[Source]) -> Result<CanonicalRecording, Error> {
    if duration_frames == 0
        || duration_frames > MAX_FRAMES
        || sources.len() > MAX_SOURCES
        || sources
            .iter()
            .map(|source| source.samples.len() as u64)
            .sum::<u64>()
            > MAX_FRAMES * 2
    {
        return Err(Error::Limit);
    }
    let mut ordered = sources.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| (a.rank, &a.capture_id).cmp(&(b.rank, &b.capture_id)));
    let mut ids = std::collections::BTreeSet::new();
    let mut boundaries = vec![0, duration_frames];
    let mut gap_runs = Vec::with_capacity(ordered.len());
    for source in &ordered {
        validate_source(source)?;
        if !ids.insert(source.capture_id.as_str()) {
            return Err(Error::InvalidSource);
        }
        let offset = source.offset_frames;
        let mut add_boundary = |frame: u64| {
            boundaries.push((offset + frame as i64).clamp(0, duration_frames as i64) as u64);
        };
        add_boundary(0);
        add_boundary(source.samples.len() as u64);
        let mut gaps = source.gaps.clone();
        gaps.sort_unstable_by_key(|gap| (gap.start, gap.end));
        let mut merged: Vec<FrameRange> = Vec::with_capacity(gaps.len());
        for gap in gaps {
            if let Some(last) = merged.last_mut()
                && last.end >= gap.start
            {
                last.end = last.end.max(gap.end);
            } else {
                merged.push(gap);
            }
        }
        for gap in &merged {
            add_boundary(gap.start);
            add_boundary(gap.end);
        }
        gap_runs.push(merged);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut samples = vec![0i16; duration_frames as usize];
    let mut selections: Vec<Selection> = Vec::new();
    for interval in boundaries.windows(2) {
        let (start, end) = (interval[0], interval[1]);
        let selected = ordered.iter().enumerate().find_map(|(index, source)| {
            let local_start = start as i64 - source.offset_frames;
            let local_end = end as i64 - source.offset_frames;
            if local_start < 0 || local_end > source.samples.len() as i64 {
                return None;
            }
            let local_start = local_start as u64;
            let local_end = local_end as u64;
            let gaps = &gap_runs[index];
            let next_gap = gaps.partition_point(|gap| gap.end <= local_start);
            if gaps.get(next_gap).is_some_and(|gap| gap.start < local_end) {
                return None;
            }
            Some((*source, local_start))
        });
        let selection = if let Some((source, source_start)) = selected {
            samples[start as usize..end as usize].copy_from_slice(
                &source.samples[source_start as usize..(source_start + end - start) as usize],
            );
            Selection {
                start_frame: start,
                end_frame: end,
                capture_id: Some(source.capture_id.clone()),
                source_sha256: Some(source.sha256.clone()),
                source_start_frame: Some(source_start),
            }
        } else {
            Selection {
                start_frame: start,
                end_frame: end,
                capture_id: None,
                source_sha256: None,
                source_start_frame: None,
            }
        };
        if let Some(last) = selections.last_mut()
            && last.capture_id == selection.capture_id
            && last.source_sha256 == selection.source_sha256
            && last
                .source_start_frame
                .map(|frame| frame + last.end_frame - last.start_frame)
                == selection.source_start_frame
        {
            last.end_frame = end;
        } else {
            selections.push(selection);
        }
    }
    let manifest = Manifest {
        version: 1,
        sample_rate: SAMPLE_RATE,
        duration_frames,
        selections,
    };
    let revision = sha256(&serde_json::to_vec(&manifest)?);
    let wav = encode_wav(&samples)?;
    Ok(CanonicalRecording {
        sha256: sha256(&wav),
        wav,
        revision,
        manifest,
    })
}

pub fn encode_wav(samples: &[i16]) -> Result<Vec<u8>, Error> {
    if samples.is_empty() || samples.len() as u64 > MAX_FRAMES {
        return Err(Error::Limit);
    }
    let mut buffer = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(
        &mut buffer,
        hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )?;
    for sample in samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()?;
    Ok(buffer.into_inner())
}

#[cfg(test)]
mod tests;
