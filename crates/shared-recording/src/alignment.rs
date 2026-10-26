use std::collections::BTreeMap;

use anlg_audio_sync::GccPhatLagEstimator;

use crate::{SAMPLE_RATE, Source};

// Clock hints locate overlap; they never authorize a splice. Keep one capture as
// the reference and admit other segments only after independent audio anchors.
pub fn align_clock_hints(sources: Vec<(Source, String, bool)>) -> Vec<Source> {
    let mut groups: BTreeMap<String, u64> = BTreeMap::new();
    for (source, group, _) in &sources {
        let mut gaps = source.gaps.clone();
        gaps.sort_by_key(|gap| gap.start);
        let mut end = 0;
        let mut missing = 0;
        for gap in gaps {
            missing += gap.end.saturating_sub(end.max(gap.start));
            end = end.max(gap.end);
        }
        *groups.entry(group.clone()).or_default() +=
            (source.samples.len() as u64).saturating_sub(missing);
    }
    let Some(reference_group) = groups
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(group, _)| group)
    else {
        return Vec::new();
    };
    let references = sources
        .iter()
        .filter(|(_, group, _)| group == reference_group)
        .map(|(source, _, _)| source)
        .collect::<Vec<_>>();
    let mut result = Vec::new();
    for (source, group, trusted) in &sources {
        if group == reference_group || *trusted {
            result.push(source.clone());
            continue;
        }
        if let Some(correction) = references
            .iter()
            .find_map(|reference| alignment(reference, source))
        {
            let mut aligned = source.clone();
            aligned.offset_frames -= correction;
            result.push(aligned);
        }
    }
    result
}

fn alignment(reference: &Source, observed: &Source) -> Option<i64> {
    let window = SAMPLE_RATE as usize * 4;
    let search = SAMPLE_RATE as usize * 2;
    let start = reference.offset_frames.max(observed.offset_frames);
    let end = (reference.offset_frames + reference.samples.len() as i64)
        .min(observed.offset_frames + observed.samples.len() as i64);
    if end - start < (window * 3) as i64 {
        return None;
    }
    let energy = |samples: &[i16]| {
        samples.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / samples.len() as f64
    };
    // Locate informative, independent windows before bounding the FFT work;
    // uniform probes can otherwise miss speech between long silent intervals.
    let positions = (start..=end - window as i64)
        .step_by(window)
        .filter(|&position| {
            let a = (position - reference.offset_frames) as usize;
            let b = (position - observed.offset_frames) as usize;
            !has_gap(reference, a, window)
                && !has_gap(observed, b, window)
                && energy(&reference.samples[a..a + window]) >= 10000.0
                && energy(&observed.samples[b..b + window]) >= 10000.0
        })
        .collect::<Vec<_>>();
    if positions.len() < 2 {
        return None;
    }
    let mut estimator = GccPhatLagEstimator::new(window, search);
    let mut lags = Vec::new();
    let anchors = positions.len().min(32);
    for index in 0..anchors {
        let position = positions[index * (positions.len() - 1) / (anchors - 1)];
        let reference_start = (position - reference.offset_frames) as usize;
        let observed_start = (position - observed.offset_frames) as usize;
        let a = &reference.samples[reference_start..reference_start + window];
        let b = &observed.samples[observed_start..observed_start + window];
        let a = a.iter().map(|&x| x as f32 / 32768.0).collect::<Vec<_>>();
        let b = b.iter().map(|&x| x as f32 / 32768.0).collect::<Vec<_>>();
        let Some(estimate) = estimator.estimate(&a, &b) else {
            continue;
        };
        if estimate.peak_ratio < 12.0
            || estimate.distinctiveness < 2.0
            || estimate.lag_samples.unsigned_abs() >= search
        {
            continue;
        }
        let lag = estimate.lag_samples;
        let (a, b) = if lag >= 0 {
            (&a[..window - lag as usize], &b[lag as usize..])
        } else {
            (&a[lag.unsigned_abs()..], &b[..window - lag.unsigned_abs()])
        };
        let dot: f64 = a.iter().zip(b).map(|(&a, &b)| a as f64 * b as f64).sum();
        let ea: f64 = a.iter().map(|&a| (a as f64).powi(2)).sum();
        let eb: f64 = b.iter().map(|&b| (b as f64).powi(2)).sum();
        if dot / (ea * eb).sqrt().max(f64::EPSILON) < 0.85 {
            continue;
        }
        lags.push(lag as i64);
    }
    if lags.len() < 2 || lags.iter().max()? - lags.iter().min()? > 16 {
        return None;
    }
    lags.sort_unstable();
    Some(lags[lags.len() / 2])
}

fn has_gap(source: &Source, start: usize, length: usize) -> bool {
    source
        .gaps
        .iter()
        .any(|gap| gap.start < (start + length) as u64 && gap.end > start as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FrameRange;

    #[test]
    fn only_audio_proven_overlap_can_fill_another_devices_gap() {
        let mut state = 42u32;
        let samples = (0..SAMPLE_RATE as usize * 24)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as i16) / 2
            })
            .collect::<Vec<_>>();
        let reference = Source {
            capture_id: "a".into(),
            sha256: "a".repeat(64),
            offset_frames: 0,
            rank: 0,
            samples: samples.clone(),
            gaps: vec![FrameRange {
                start: 160000,
                end: 180000,
            }],
        };
        let mut observed = reference.clone();
        observed.capture_id = "b".into();
        observed.gaps.clear();
        observed.samples = samples[8000..].to_vec();
        observed.offset_frames = 9000;
        let selected = align_clock_hints(vec![
            (reference.clone(), "a".into(), false),
            (observed.clone(), "b".into(), false),
        ]);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[1].offset_frames - selected[0].offset_frames, 8000);
        observed.samples.fill(0);
        assert_eq!(
            align_clock_hints(vec![
                (reference, "a".into(), false),
                (observed, "b".into(), false)
            ])
            .len(),
            1
        );
    }
    #[test]
    fn silent_overlap_edges_do_not_discard_complementary_speech() {
        let rate = SAMPLE_RATE as usize;
        let mut state = 42u32;
        let samples = (0..rate * 96)
            .map(|index| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                if index < rate * 12
                    || (rate * 36..rate * 52).contains(&index)
                    || (rate * 70..rate * 82).contains(&index)
                {
                    (state as i16) / 2
                } else {
                    0
                }
            })
            .collect::<Vec<_>>();
        let earlier = crate::decode_source(
            "a".into(),
            0,
            5000,
            &crate::encode_wav(&samples[..rate * 60]).unwrap(),
            vec![],
        )
        .unwrap();
        let later = crate::decode_source(
            "b".into(),
            0,
            (rate * 30) as i64,
            &crate::encode_wav(&samples[rate * 30..]).unwrap(),
            vec![],
        )
        .unwrap();
        let selected = align_clock_hints(vec![
            (earlier.clone(), "a".into(), false),
            (later.clone(), "b".into(), false),
        ]);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].offset_frames, 0);
        let canonical = crate::render(samples.len() as u64, &selected).unwrap();
        let rendered = hound::WavReader::new(std::io::Cursor::new(canonical.wav))
            .unwrap()
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rendered, samples);

        let mut unrelated = earlier;
        for sample in &mut unrelated.samples {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *sample = (state as i16) / 2;
        }
        assert_eq!(
            align_clock_hints(vec![
                (unrelated, "a".into(), false),
                (later, "b".into(), false),
            ])
            .len(),
            1
        );
    }
}
