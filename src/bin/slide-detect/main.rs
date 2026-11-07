//! `slide-detect`: the slides of a lecture video, from the command line.
//!
//! Exit codes: 0 on success, 1 on failure, 2 on a usage error, 130 when cancelled
//! by Ctrl-C (SIGINT), 143 when terminated (SIGTERM).

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
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Parser)]
#[command(
    name = "slide-detect",
    version,
    about = "Find the slide changes in a lecture video, and extract a full-resolution image of each",
    after_help = "Needs ffmpeg: on PATH, or given with --ffmpeg.\n\
                  Exit codes: 0 success, 1 failure, 2 usage error, 130 Ctrl-C, 143 SIGTERM."
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
    let signal = Arc::new(AtomicUsize::new(0));
    if let Err(error) = listen(&signal) {
        progress::say(&format!(
            "slide-detect: warning: Ctrl-C will not cancel cleanly: {error}"
        ));
    }
    match run(cli, &signal) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Cancelled) => {
            let signal = match signal.load(Ordering::Relaxed) {
                0 => SIGINT,
                signal => signal,
            };
            let word = if signal == SIGTERM {
                "terminated"
            } else {
                "cancelled"
            };
            progress::say(&format!("slide-detect: {word}"));
            // The shell's convention for a death by signal.
            ExitCode::from(128 + signal as u8)
        }
        Err(Failure::Error(message)) => {
            progress::say(&format!("slide-detect: error: {message}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
use signal_hook::consts::{SIGINT as SIGINT_C, SIGTERM as SIGTERM_C};
#[cfg(unix)]
const SIGINT: usize = SIGINT_C as usize;
#[cfg(unix)]
const SIGTERM: usize = SIGTERM_C as usize;
#[cfg(not(unix))]
const SIGINT: usize = 2;
#[cfg(not(unix))]
const SIGTERM: usize = 15;

/// Stores the number of the first stopping signal in `signal`; the library sees it
/// through `cancelled` and stops every ffmpeg.
#[cfg(unix)]
fn listen(signal: &Arc<AtomicUsize>) -> std::io::Result<()> {
    for number in [SIGINT_C, SIGTERM_C] {
        signal_hook::flag::register_usize(number, Arc::clone(signal), number as usize)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn listen(signal: &Arc<AtomicUsize>) -> std::io::Result<()> {
    let signal = Arc::clone(signal);
    ctrlc::set_handler(move || signal.store(SIGINT, Ordering::Relaxed))
        .map_err(std::io::Error::other)
}

fn run(cli: Cli, cancelled: &AtomicUsize) -> Result<(), Failure> {
    match cli.command {
        Command::Scan { common } => {
            let times = Job::prepare(&common, cancelled)?.run(scan)?;
            let text = if common.json {
                output::scan_json(&common.video, &times)
            } else {
                output::scan_table(&times)
            };
            print(&text)
        }
        Command::Extract { common, output } => {
            // Probe first: a mistyped video name creates no directory.
            let job = Job::prepare(&common, cancelled)?;
            std::fs::create_dir_all(&output)
                .map_err(|error| format!("could not create {}: {error}", output.display()))?;
            // Detect into a hidden directory beside the images, so a failed or
            // cancelled run leaves nothing half done, and renaming stays on one disk.
            let staging = tempfile::Builder::new()
                .prefix(".slide-detect-")
                .tempdir_in(&output)
                .map_err(|error| format!("could not write to {}: {error}", output.display()))?;
            let slides = job.run(|ffmpeg, video, info, params, c, e| {
                detect(ffmpeg, video, info, params, staging.path(), c, e)
            })?;
            let files: Vec<_> = slides
                .iter()
                .enumerate()
                .map(|(index, slide)| output::image_name(index + 1, slide.onset))
                .collect();
            check_replaceable(&output, &files)?;
            for (slide, name) in slides.iter().zip(&files) {
                let dest = output.join(name);
                std::fs::rename(staging.path().join(&slide.file_name), &dest)
                    .map_err(|error| format!("could not write {}: {error}", dest.display()))?;
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

/// Every image's place must take a file before the first is moved, so a run does not
/// stop with the directory half replaced. The directory itself is writable: the
/// staging directory is in it.
fn check_replaceable(dir: &Path, names: &[String]) -> Result<(), Failure> {
    for name in names {
        let dest = dir.join(name);
        let problem = match std::fs::symlink_metadata(&dest) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => error.to_string(),
            Ok(meta) if meta.is_dir() => "a directory is in the way".to_owned(),
            Ok(meta) if meta.permissions().readonly() => "the file is read-only".to_owned(),
            Ok(_) => continue,
        };
        return Err(format!(
            "cannot write {}: {problem}; no image was written",
            dest.display()
        )
        .into());
    }
    Ok(())
}

/// A video ready for the library: ffmpeg found, the video probed.
struct Job<'a> {
    common: &'a Common,
    cancelled: &'a AtomicUsize,
    ffmpeg: PathBuf,
    info: VideoInfo,
}

impl<'a> Job<'a> {
    /// The checks and probe both commands need, before either writes anything.
    fn prepare(common: &'a Common, cancelled: &'a AtomicUsize) -> Result<Self, Failure> {
        let ffmpeg = ffmpeg::locate(common.ffmpeg.as_deref())?;
        let video = &common.video;
        if !video.exists() {
            return Err(format!("{}: no such file", video.display()).into());
        }
        if video.is_dir() {
            return Err(format!("{}: is a directory, not a video", video.display()).into());
        }
        let is_cancelled = || cancelled.load(Ordering::Relaxed) != 0;
        // A signal during the probe stops its ffmpeg, and is then the reason it failed.
        let info = ffmpeg::probe(&ffmpeg, video, &is_cancelled).map_err(|message| {
            if is_cancelled() {
                Failure::Cancelled
            } else {
                Failure::Error(message)
            }
        })?;
        Ok(Self {
            common,
            cancelled,
            ffmpeg,
            info,
        })
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed) != 0
    }

    /// Runs `call`, [`scan`] or [`detect`], with progress and cancelling.
    fn run<T>(
        &self,
        call: impl FnOnce(
            &Path,
            &Path,
            VideoInfo,
            &SlideParams,
            &(dyn Fn() -> bool + Sync),
            &mut dyn FnMut(Event),
        ) -> Result<T, Error>,
    ) -> Result<T, Failure> {
        let common = self.common;
        let params = SlideParams {
            sample_interval: common.sample_interval,
            similarity: common.similarity,
            cooldown: common.cooldown,
            stable_secs: common.stable_secs,
            roi: common.roi,
        };
        let mut progress = Progress::new(common.quiet, self.info.duration > 0.0);
        let result = call(
            &self.ffmpeg,
            &common.video,
            self.info,
            &params,
            &|| self.is_cancelled(),
            &mut |event| progress.event(event),
        );
        progress.finish();
        // A Ctrl-C at a terminal reaches ffmpeg too, which may then fail before the
        // library sees the flag: the user's cancel is the reason either way.
        if self.is_cancelled() {
            return Err(Failure::Cancelled);
        }
        result.map_err(|error| describe(error, &self.ffmpeg, &common.video))
    }
}

fn describe(error: Error, ffmpeg: &Path, video: &Path) -> Failure {
    let video = video.display();
    Failure::Error(match error {
        Error::Cancelled => return Failure::Cancelled,
        Error::Spawn(error) => format!("could not start {}: {error}", ffmpeg.display()),
        Error::ScanFailed { status, stderr } => {
            format!("ffmpeg could not read {video} ({status}):\n{stderr}")
        }
        Error::NoFrames => format!(
            "no frame was sampled from {video}; the video may be shorter than --sample-interval"
        ),
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
