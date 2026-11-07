//! Finding ffmpeg, and asking it the video's size and duration.

use slide_detect::VideoInfo;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// How often the probe looks at `cancelled`.
const POLL: Duration = Duration::from_millis(20);

/// How many lines of ffmpeg's output to quote when the probe fails.
const STDERR_LINES: usize = 5;

/// `explicit` if given (a bare name is looked up on PATH, as a shell would), else
/// `ffmpeg` on PATH.
pub fn locate(explicit: Option<&Path>) -> Result<PathBuf, String> {
    let Some(path) = explicit else {
        return search(OsStr::new("ffmpeg")).ok_or_else(|| {
            "ffmpeg not found on PATH; install it (https://ffmpeg.org) or pass --ffmpeg <PATH>"
                .to_owned()
        });
    };
    if path.is_file() {
        return Ok(path.to_owned());
    }
    let bare = path
        .parent()
        .is_none_or(|parent| parent.as_os_str().is_empty());
    bare.then(|| search(path.as_os_str()))
        .flatten()
        .ok_or_else(|| format!("ffmpeg not found at {}", path.display()))
}

fn search(name: &OsStr) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| candidates(dir.join(name)))
        .find(|candidate| candidate.is_file())
}

#[cfg(windows)]
fn candidates(path: PathBuf) -> Vec<PathBuf> {
    let exe = path.with_extension("exe");
    vec![path, exe]
}

#[cfg(not(windows))]
fn candidates(path: PathBuf) -> Vec<PathBuf> {
    vec![path]
}

/// Runs `ffmpeg -i`, which describes the input on stderr and exits. Once
/// `cancelled` returns true ffmpeg is killed, and the caller reports the cancel.
pub fn probe(
    ffmpeg: &Path,
    video: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<VideoInfo, String> {
    let mut child = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(video)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", ffmpeg.display()))?;
    let stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_end(&mut text);
        }
        text
    });
    loop {
        if cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cancelled".to_owned());
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(POLL),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("could not run {}: {error}", ffmpeg.display()));
            }
        }
    }
    let text = reader.join().unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    parse(&text).ok_or_else(|| {
        let lines: Vec<_> = text
            .lines()
            .filter(|line| !line.contains("output file must be specified"))
            .collect();
        let tail = lines[lines.len().saturating_sub(STDERR_LINES)..].join("\n");
        format!(
            "ffmpeg found no video stream in {}:\n{tail}",
            video.display()
        )
    })
}

/// The first video stream's size, and the duration if known (0 otherwise: it only
/// estimates progress).
fn parse(text: &str) -> Option<VideoInfo> {
    let (width, height) = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("Stream #") && !line.contains("(attached pic)"))
        .filter_map(|line| line.split_once("Video:").map(|(_, rest)| rest))
        .find_map(size)?;
    Some(VideoInfo {
        width,
        height,
        duration: duration(text).unwrap_or(0.0),
    })
}

/// `1280x720` among the comma-separated fields: the first word of one of them.
fn size(fields: &str) -> Option<(u32, u32)> {
    fields.split(',').find_map(|field| {
        let word = field.split_whitespace().next()?;
        let (w, h) = word.split_once('x')?;
        let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if !all_digits(w) || !all_digits(h) {
            return None;
        }
        let (w, h) = (w.parse().ok()?, h.parse().ok()?);
        (w > 0 && h > 0).then_some((w, h))
    })
}

/// `Duration: 01:02:03.45,` in seconds.
fn duration(text: &str) -> Option<f64> {
    let (_, rest) = text.split_once("Duration: ")?;
    let stamp = rest.split(',').next()?.trim();
    let mut parts = stamp.split(':');
    let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
    let secs =
        h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?;
    (secs.is_finite() && secs >= 0.0).then_some(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MP4: &str = "\
Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'lecture.mp4':
  Duration: 01:02:03.50, start: 0.000000, bitrate: 11 kb/s
  Stream #0:0[0x1](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp
  Stream #0:1[0x2](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(tv, bt709, progressive), 1920x1080 [SAR 1:1 DAR 16:9], 6 kb/s, 25 fps
At least one output file must be specified";

    #[test]
    fn reads_the_size_and_duration() {
        assert_eq!(
            parse(MP4),
            Some(VideoInfo {
                width: 1920,
                height: 1080,
                duration: 3723.5
            })
        );
    }

    #[test]
    fn an_unknown_duration_is_zero() {
        let text = "  Duration: N/A, bitrate: N/A\n  Stream #0:0: Video: vp9, yuv420p, 640x360";
        assert_eq!(parse(text).map(|info| info.duration), Some(0.0));
    }

    #[test]
    fn audio_and_cover_art_are_not_video() {
        let text = "  Duration: 00:00:10.00\n  Stream #0:0: Audio: mp3, 44100 Hz\n  \
                    Stream #0:1: Video: mjpeg, yuvj420p, 500x500, 90k tbn (attached pic)";
        assert_eq!(parse(text), None);
        assert_eq!(parse("Invalid data found when processing input"), None);
    }

    #[test]
    fn a_codec_tag_is_not_a_size() {
        assert_eq!(size(" h264 (avc1 / 0x31637661)"), None);
        assert_eq!(size(" h264 (avc1 / 0x31637661), 320x240"), Some((320, 240)));
    }

    #[test]
    fn a_missing_ffmpeg_is_named() {
        let error = locate(Some(Path::new("/no/such/ffmpeg"))).unwrap_err();
        assert_eq!(error, "ffmpeg not found at /no/such/ffmpeg");
    }
}
