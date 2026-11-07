//! Finds the slide changes in a video, and extracts a full-resolution JPEG of each.
//!
//! ffmpeg decodes a small grayscale stream, one frame every
//! [`SlideParams::sample_interval`] seconds. A frame that differs from the last
//! slide by SSIM becomes a candidate; it becomes a slide once it has held for
//! [`SlideParams::stable_secs`] and [`SlideParams::cooldown`] has passed since the
//! last slide. Detection never sleeps during a cooldown, so a slide's time is when
//! it first appeared, not when the cooldown ended.
//!
//! The caller supplies the ffmpeg path and the video's dimensions and duration, and
//! receives typed [`Event`]s and [`Error`]s: the crate has no user-facing text.

mod detector;
mod extract;
mod scan;

use std::fmt;
use std::path::{Path, PathBuf};

/// Detection settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlideParams {
    /// Seconds between sampled frames.
    pub sample_interval: f64,
    /// SSIM below this (0 to 1) counts as a different picture.
    pub similarity: f64,
    /// Minimum seconds between two slides.
    pub cooldown: f64,
    /// How long a picture must hold to count; 0 accepts it at once. Above 0, a
    /// transition that never settles produces no slide.
    pub stable_secs: f64,
    /// Compare only this region; everything else may change freely.
    pub roi: Option<Roi>,
}

/// A region of the frame, as fractions of its width and height (0 to 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Roi {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

/// The video to scan, as the caller's probe reported it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    /// Seconds; only used to estimate progress.
    pub duration: f64,
}

/// When one slide shows, as [`scan`] finds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlideTime {
    /// When the picture first appeared: the time to align a transcript with.
    pub onset: f64,
    /// The last sample of the settled picture: where its image is taken.
    pub capture: f64,
}

/// One detected slide. The image is `file_name` in the output directory.
#[derive(Debug, Clone, PartialEq)]
pub struct Slide {
    /// When the picture first appeared: the time to align a transcript with.
    pub onset: f64,
    /// When the image was taken: the last sample of the settled picture.
    pub capture: f64,
    /// `slide_0001.jpg`, numbered from 1 in time order.
    pub file_name: String,
}

/// What [`scan`] or [`detect`] is doing, in order: one `ScanStarted`, any number of
/// `Sampled` and `CandidateFound`, one `ScanFinished`; then, for [`detect`] only and
/// if anything was found, one `ExtractStarted` and one `Extracted` per slide.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// `estimate` is the frame count the duration suggests, not a promise: variable
    /// frame rate input may land a frame or two away.
    ScanStarted {
        estimate: u64,
    },
    Sampled {
        frames: u64,
    },
    CandidateFound {
        slides: usize,
    },
    ScanFinished {
        frames: u64,
        slides: usize,
    },
    ExtractStarted {
        slides: usize,
    },
    Extracted {
        done: usize,
        slides: usize,
    },
}

/// Why [`scan`] or [`detect`] stopped.
#[derive(Debug)]
pub enum Error {
    /// `cancelled` returned true.
    Cancelled,
    /// ffmpeg could not start for the scan.
    Spawn(std::io::Error),
    /// The scan's ffmpeg failed; `stderr` holds the end of its output.
    ScanFailed {
        status: std::process::ExitStatus,
        stderr: String,
    },
    /// The scan ended with no slide, as for a video with no frames.
    NoFrames,
    /// The image of slide `number` (from 1), which first appeared at `onset`, failed.
    Extract {
        number: usize,
        onset: f64,
        source: ExtractError,
    },
    Io(std::io::Error),
}

/// Why one image could not be extracted.
#[derive(Debug)]
pub enum ExtractError {
    Spawn(std::io::Error),
    /// ffmpeg failed; `stderr` holds the end of its output.
    Failed {
        code: Option<i32>,
        stderr: String,
    },
    /// ffmpeg reported success and the file is not there.
    Missing(PathBuf),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("cancelled"),
            Self::Spawn(_) => f.write_str("could not start ffmpeg"),
            Self::ScanFailed { status, stderr } => {
                write!(f, "ffmpeg failed ({status}): {stderr}")
            }
            Self::NoFrames => f.write_str("no frames sampled"),
            Self::Extract { number, onset, .. } => {
                write!(f, "could not extract slide {number} (t={onset:.1}s)")
            }
            Self::Io(_) => f.write_str("frame stream or output directory failed"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) | Self::Io(error) => Some(error),
            Self::Extract { source, .. } => Some(source),
            Self::Cancelled | Self::ScanFailed { .. } | Self::NoFrames => None,
        }
    }
}

impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "could not start ffmpeg: {error}"),
            Self::Failed { code, stderr } => write!(f, "ffmpeg failed ({code:?}): {stderr}"),
            Self::Missing(path) => write!(f, "ffmpeg did not produce {}", path.display()),
        }
    }
}

impl std::error::Error for ExtractError {}

/// Finds when each slide of `video` shows, without extracting any image: the first
/// pass of [`detect`]. Blocking. `cancelled` is polled about ten times a second;
/// once it returns true ffmpeg is killed and [`Error::Cancelled`] returned.
/// `on_event` runs on the calling thread, and receives the scan's events only.
/// A scan that finds nothing is [`Error::NoFrames`], so the result is never empty.
pub fn scan(
    ffmpeg: &Path,
    video: &Path,
    info: VideoInfo,
    params: &SlideParams,
    cancelled: &(dyn Fn() -> bool + Sync),
    on_event: &mut dyn FnMut(Event),
) -> Result<Vec<SlideTime>, Error> {
    let times = scan::scan(ffmpeg, video, info, params, cancelled, on_event)?;
    if times.is_empty() {
        return Err(Error::NoFrames);
    }
    Ok(times
        .into_iter()
        .map(|(onset, capture)| SlideTime { onset, capture })
        .collect())
}

/// Detects the slides of `video` and writes their images to `output_dir`, created if
/// needed: [`scan`], then one full-resolution JPEG per slide. Blocking. `cancelled`
/// is polled about ten times a second; once it returns true every ffmpeg is killed
/// and [`Error::Cancelled`] returned. `on_event` runs on the calling thread.
pub fn detect(
    ffmpeg: &Path,
    video: &Path,
    info: VideoInfo,
    params: &SlideParams,
    output_dir: &Path,
    cancelled: &(dyn Fn() -> bool + Sync),
    on_event: &mut dyn FnMut(Event),
) -> Result<Vec<Slide>, Error> {
    std::fs::create_dir_all(output_dir).map_err(Error::Io)?;
    let times = scan(ffmpeg, video, info, params, cancelled, on_event)?;
    extract::extract(ffmpeg, video, &times, output_dir, cancelled, on_event)
}

/// How often a blocked wait looks at `cancelled`.
const POLL: std::time::Duration = std::time::Duration::from_millis(100);
