use super::*;

fn source(id: &str, rank: u32, offset: i64, samples: &[i16], gaps: Vec<FrameRange>) -> Source {
    decode_source(id.into(), rank, offset, &encode_wav(samples).unwrap(), gaps).unwrap()
}

fn decoded(wav: &[u8]) -> Vec<i16> {
    hound::WavReader::new(Cursor::new(wav))
        .unwrap()
        .samples::<i16>()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn fallback_fills_only_declared_gaps_and_preserves_silence_without_summing() {
    let primary = source(
        "alice",
        0,
        0,
        &[1, 0, 3, 4, 5, 6],
        vec![
            FrameRange { start: 3, end: 4 },
            FrameRange { start: 2, end: 4 },
            FrameRange { start: 2, end: 3 },
        ],
    );
    let fallback = source("bob", 1, 1, &[10, 20, 30, 40, 50, 60], vec![]);
    let result = render(8, &[fallback.clone(), primary.clone()]).unwrap();
    assert_eq!(decoded(&result.wav), vec![1, 0, 20, 30, 5, 6, 60, 0]);
    assert_eq!(result.manifest.selections.last().unwrap().capture_id, None);
    let reversed = render(8, &[primary, fallback]).unwrap();
    assert_eq!(result.wav, reversed.wav);
    assert_eq!(result.revision, reversed.revision);
}

#[test]
fn unused_late_inputs_do_not_change_revision_but_new_coverage_does() {
    let primary = source("alice", 0, 0, &[1, 2, 3, 4], vec![]);
    let before = render(6, std::slice::from_ref(&primary)).unwrap();
    let unused = source("bob", 1, 1, &[9, 9], vec![]);
    let same = render(6, &[primary.clone(), unused]).unwrap();
    assert_eq!(before.revision, same.revision);
    assert_eq!(before.sha256, same.sha256);
    let late = source("carol", 1, 4, &[5, 6], vec![]);
    let improved = render(6, &[primary, late]).unwrap();
    assert_eq!(decoded(&improved.wav), vec![1, 2, 3, 4, 5, 6]);
    assert_ne!(before.revision, improved.revision);
    assert_ne!(before.sha256, improved.sha256);
}

#[test]
fn negative_offset_clips_leading_frames_and_rank_ties_use_capture_identity() {
    let z = source("z", 0, -2, &[1, 2, 3, 4], vec![]);
    let a = source("a", 0, 1, &[8, 9], vec![]);
    let result = render(4, &[z, a]).unwrap();
    assert_eq!(decoded(&result.wav), vec![3, 8, 9, 0]);
    assert_eq!(result.manifest.selections[0].source_start_frame, Some(2));
}

#[test]
fn invalid_formats_ranges_duplicate_sources_and_excessive_timelines_are_rejected() {
    let good = source("a", 0, 0, &[1, 2], vec![]);
    assert!(render(2, &[good.clone(), good.clone()]).is_err());
    assert!(render(MAX_FRAMES + 1, &[]).is_err());
    let mut invalid = good;
    invalid.gaps.push(FrameRange { start: 1, end: 3 });
    assert!(render(2, &[invalid]).is_err());
    let mut bytes = encode_wav(&[1, 2]).unwrap();
    bytes[24..28].copy_from_slice(&48_000u32.to_le_bytes());
    assert!(decode_source("a".into(), 0, 0, &bytes, vec![]).is_err());
    assert!(decode_source("a".into(), 0, 0, &bytes[..20], vec![]).is_err());
}
