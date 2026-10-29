use tauri::{Result, image::Image};

pub enum TrayIconState {
    Default,
    Degraded,
    UpdateAvailable,
}

pub const RECORDING_FRAMES: &[&[u8]] = &[
    include_bytes!("../icons/tray_recording_0.png"),
    include_bytes!("../icons/tray_recording_1.png"),
    include_bytes!("../icons/tray_recording_2.png"),
];

impl TrayIconState {
    pub fn to_image(&self, scribe: bool) -> Result<Image<'static>> {
        if scribe {
            return Ok(scribe_image(
                &[10.0, 20.0, 30.0, 20.0, 10.0],
                match self {
                    Self::Default => None,
                    Self::Degraded => Some(false),
                    Self::UpdateAvailable => Some(true),
                },
            ));
        }
        match self {
            TrayIconState::Default => {
                Image::from_bytes(include_bytes!("../icons/tray_default.png"))
            }
            TrayIconState::Degraded => {
                Image::from_bytes(include_bytes!("../icons/tray_degraded.png"))
            }
            TrayIconState::UpdateAvailable => {
                Image::from_bytes(include_bytes!("../icons/tray_update.png"))
            }
        }
    }
}

pub fn recording_image(frame: usize, scribe: bool) -> Result<Image<'static>> {
    if !scribe {
        return Image::from_bytes(RECORDING_FRAMES[frame]);
    }
    let heights = [
        [12.0, 24.0, 34.0, 24.0, 12.0],
        [20.0, 34.0, 20.0, 12.0, 24.0],
        [30.0, 14.0, 24.0, 34.0, 18.0],
    ];
    Ok(scribe_image(&heights[frame], None))
}

// Render with rounded ends and antialiasing before macOS scales to menu-bar size.
// Alpha-only artwork lets macOS supply the correct light/dark appearance.
fn scribe_image(heights: &[f64; 5], badge: Option<bool>) -> Image<'static> {
    let mut rgba = vec![0; 44 * 44 * 4];
    for y in 0..44 {
        for x in 0..44 {
            let mut coverage = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0;
                    let waveform = heights.iter().enumerate().any(|(i, height)| {
                        let dx = px - (8.0 + i as f64 * 7.0);
                        let dy = (py - 22.0).abs() - (height / 2.0 - 2.0);
                        dx * dx + dy.max(0.0).powi(2) <= 4.0
                    });
                    let marked = badge.is_some_and(|update| {
                        let dx = px - 35.0;
                        let dy = py - 9.0;
                        if update {
                            dx * dx + dy * dy <= 9.0
                        } else {
                            dx.abs() <= 1.25
                                && ((-4.0..=0.5).contains(&dy) || (2.5..=4.5).contains(&dy))
                        }
                    });
                    coverage += u8::from(waveform || marked);
                }
            }
            rgba[(y * 44 + x) * 4 + 3] = (coverage as u16 * 255 / 16) as u8;
        }
    }
    Image::new_owned(rgba, 44, 44)
}
