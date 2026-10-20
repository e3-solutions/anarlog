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
    let mut estimator = GccPhatLagEstimator::new(window, search);
    let mut lags = Vec::new();
    for position in [
        start,
        start + (end - start - window as i64) / 2,
        end - window as i64,
    ] {
        let reference_start = (position - reference.offset_frames) as usize;
        let observed_start = (position - observed.offset_frames) as usize;
        if has_gap(reference, reference_start, window) || has_gap(observed, observed_start, window)
        {
            continue;
        }
        let a = &reference.samples[reference_start..reference_start + window];
        let b = &observed.samples[observed_start..observed_start + window];
        let energy = |samples: &[i16]| {
            samples.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / samples.len() as f64
        };
        if energy(a) < 10000.0 || energy(b) < 10000.0 {
            continue;
        }
        let a = a.iter().map(|&x| x as f32 / 32768.0).collect::<Vec<_>>();
        let b = b.iter().map(|&x| x as f32 / 32768.0).collect::<Vec<_>>();
        let estimate = estimator.estimate(&a, &b)?;
        if estimate.peak_ratio < 12.0
            || estimate.distinctiveness < 2.0
            || estimate.lag_samples.unsigned_abs() >= search
        {
            return None;
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
            return None;
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
}
