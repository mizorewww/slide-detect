//! `slide-detect`: the slides of a lecture video, from the command line.
//!
//! Exit codes: 0 on success, 1 on failure, 2 on a usage error, 130 when cancelled.

mod ffmpeg;
mod output;
mod progress;

use clap::{Args, Parser, Subcommand};
use progress::Progress;
use slide_detect::{Error, Event, Roi, SlideParams, VideoInfo, detect, scan};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Parser)]
#[command(
    name = "slide-detect",
    version,
    about = "Find the slide changes in a lecture video, and extract a full-resolution image of each",
    after_help = "Needs ffmpeg: on PATH, or given with --ffmpeg.\n\
                  Exit codes: 0 success, 1 failure, 2 usage error, 130 cancelled."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print when each slide appears, without extracting any image
    Scan {
        #[command(flatten)]
        common: Common,
    },
    /// Write a full-resolution JPEG of each slide to a directory
    ///
    /// Images are named by number and onset, as in slide_0002_00-01-07.500.jpg, and
    /// replace files of the same name.
    Extract {
        #[command(flatten)]
        common: Common,
        /// The directory for the images, created if needed
        #[arg(short, long, value_name = "DIR")]
        output: PathBuf,
    },
}

#[derive(Args)]
struct Common {
    /// The lecture video
    video: PathBuf,
    /// The ffmpeg to run [default: ffmpeg on PATH]
    #[arg(long, value_name = "PATH")]
    ffmpeg: Option<PathBuf>,
    /// Seconds between sampled frames
    #[arg(long, value_name = "SECS", default_value_t = 1.0, value_parser = positive)]
    sample_interval: f64,
    /// SSIM (0 to 1) below which a frame counts as a different picture
    #[arg(long, value_name = "SSIM", default_value_t = 0.85, value_parser = fraction)]
    similarity: f64,
    /// Minimum seconds between two slides
    #[arg(long, value_name = "SECS", default_value_t = 5.0, value_parser = non_negative)]
    cooldown: f64,
    /// Seconds a picture must hold to count; 0 accepts it at once
    #[arg(long, value_name = "SECS", default_value_t = 0.0, value_parser = non_negative)]
    stable_secs: f64,
    /// Compare only this region, as fractions of the frame's width and height
    #[arg(long, value_name = "X1,Y1,X2,Y2", value_parser = roi)]
    roi: Option<Roi>,
    /// Print JSON instead of a table
    #[arg(long)]
    json: bool,
    /// Show no progress on stderr
    #[arg(short, long)]
    quiet: bool,
}

/// Why a run did not succeed.
enum Failure {
    Cancelled,
    Error(String),
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::Error(message)
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    if let Err(error) = ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed)) {
        progress::say(&format!(
            "slide-detect: warning: Ctrl-C will not cancel cleanly: {error}"
        ));
    }
    match run(cli, &cancelled) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Cancelled) => {
            progress::say("slide-detect: cancelled");
            ExitCode::from(130)
        }
        Err(Failure::Error(message)) => {
            progress::say(&format!("slide-detect: error: {message}"));
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli, cancelled: &AtomicBool) -> Result<(), Failure> {
    match cli.command {
        Command::Scan { common } => {
            let times = with_library(&common, cancelled, scan)?;
            let text = if common.json {
                output::scan_json(&common.video, &times)
            } else {
                output::scan_table(&times)
            };
            print(&text)
        }
        Command::Extract { common, output } => {
            std::fs::create_dir_all(&output)
                .map_err(|error| format!("could not create {}: {error}", output.display()))?;
            // Detect into a hidden directory beside the images, so a failed or
            // cancelled run leaves nothing half done, and renaming stays on one disk.
            let staging = tempfile::Builder::new()
                .prefix(".slide-detect-")
                .tempdir_in(&output)
                .map_err(|error| format!("could not write to {}: {error}", output.display()))?;
            let slides = with_library(&common, cancelled, |ffmpeg, video, info, params, c, e| {
                detect(ffmpeg, video, info, params, staging.path(), c, e)
            })?;
            let mut files = Vec::with_capacity(slides.len());
            for (index, slide) in slides.iter().enumerate() {
                let name = output::image_name(index + 1, slide.onset);
                let dest = output.join(&name);
                std::fs::rename(staging.path().join(&slide.file_name), &dest)
                    .map_err(|error| format!("could not write {}: {error}", dest.display()))?;
                files.push(name);
            }
            let text = if common.json {
                output::extract_json(&common.video, &output, &slides, &files)
            } else {
                output::extract_table(&slides, &files)
            };
            print(&text)
        }
    }
}

/// The checks and probe both commands need, then `call`: [`scan`] or [`detect`].
fn with_library<T>(
    common: &Common,
    cancelled: &AtomicBool,
    call: impl FnOnce(
        &Path,
        &Path,
        VideoInfo,
        &SlideParams,
        &(dyn Fn() -> bool + Sync),
        &mut dyn FnMut(Event),
    ) -> Result<T, Error>,
) -> Result<T, Failure> {
    let ffmpeg = ffmpeg::locate(common.ffmpeg.as_deref())?;
    let video = &common.video;
    if !video.exists() {
        return Err(format!("{}: no such file", video.display()).into());
    }
    if video.is_dir() {
        return Err(format!("{}: is a directory, not a video", video.display()).into());
    }
    // Ctrl-C at a terminal stops the probe's ffmpeg too: then the cancel is the reason.
    let info = ffmpeg::probe(&ffmpeg, video).map_err(|message| {
        if cancelled.load(Ordering::Relaxed) {
            Failure::Cancelled
        } else {
            Failure::Error(message)
        }
    })?;
    let params = SlideParams {
        sample_interval: common.sample_interval,
        similarity: common.similarity,
        cooldown: common.cooldown,
        stable_secs: common.stable_secs,
        roi: common.roi,
    };
    let mut progress = Progress::new(common.quiet);
    let result = call(
        &ffmpeg,
        video,
        info,
        &params,
        &|| cancelled.load(Ordering::Relaxed),
        &mut |event| progress.event(event),
    );
    progress.finish();
    // A Ctrl-C at a terminal reaches ffmpeg too, which may then fail before the
    // library sees the flag: the user's cancel is the reason either way.
    if cancelled.load(Ordering::Relaxed) {
        return Err(Failure::Cancelled);
    }
    result.map_err(|error| describe(error, &ffmpeg, video))
}

fn describe(error: Error, ffmpeg: &Path, video: &Path) -> Failure {
    let video = video.display();
    Failure::Error(match error {
        Error::Cancelled => return Failure::Cancelled,
        Error::Spawn(error) => format!("could not start {}: {error}", ffmpeg.display()),
        Error::ScanFailed { status, stderr } => {
            format!("ffmpeg could not read {video} ({status}):\n{stderr}")
        }
        Error::NoFrames => format!("ffmpeg decoded no frames from {video}"),
        Error::Extract {
            number,
            onset,
            source,
        } => format!(
            "could not extract slide {number} (at {}): {source}",
            output::timestamp(onset)
        ),
        Error::Io(error) => format!("reading frames or writing images failed: {error}"),
    })
}

/// Writes to stdout; a reader that went away, as `| head` does, is not an error.
fn print(text: &str) -> Result<(), Failure> {
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => {
            Err(format!("could not write the output: {error}").into())
        }
        _ => Ok(()),
    }
}

fn number(text: &str) -> Result<f64, String> {
    match text.trim().parse::<f64>() {
        Ok(value) if value.is_finite() => Ok(value),
        _ => Err(format!("`{text}` is not a number")),
    }
}

fn positive(text: &str) -> Result<f64, String> {
    let value = number(text)?;
    if value > 0.0 {
        Ok(value)
    } else {
        Err("must be greater than 0".into())
    }
}

fn non_negative(text: &str) -> Result<f64, String> {
    let value = number(text)?;
    if value >= 0.0 {
        Ok(value)
    } else {
        Err("must not be negative".into())
    }
}

fn fraction(text: &str) -> Result<f64, String> {
    let value = number(text)?;
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err("must be between 0 and 1".into())
    }
}

fn roi(text: &str) -> Result<Roi, String> {
    let values = text
        .split(',')
        .map(fraction)
        .collect::<Result<Vec<_>, _>>()?;
    let [x1, y1, x2, y2] = values[..] else {
        return Err("needs four fractions: X1,Y1,X2,Y2".into());
    };
    if x1 >= x2 || y1 >= y2 {
        return Err("X1 must be less than X2, and Y1 less than Y2".into());
    }
    Ok(Roi { x1, y1, x2, y2 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_line_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn numbers_are_checked() {
        assert_eq!(positive("0.5"), Ok(0.5));
        assert!(positive("0").is_err());
        assert!(positive("NaN").is_err());
        assert!(positive("inf").is_err());
        assert!(non_negative("-1").is_err());
        assert!(fraction("1.5").is_err());
        assert_eq!(fraction("1"), Ok(1.0));
    }

    #[test]
    fn regions_are_four_ordered_fractions() {
        assert_eq!(
            roi("0,0.1,0.75,1"),
            Ok(Roi {
                x1: 0.0,
                y1: 0.1,
                x2: 0.75,
                y2: 1.0
            })
        );
        assert!(roi("0,0,1").is_err());
        assert!(roi("0.5,0,0.5,1").is_err());
        assert!(roi("0,0,2,1").is_err());
    }
}
