//! The first pass: a grayscale frame stream from ffmpeg, through the detector.

use crate::detector::Detector;
use crate::{Error, Event, POLL, SlideParams, VideoInfo};
use image::GrayImage;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{RecvTimeoutError, sync_channel};

/// Wide enough to tell layouts apart, and small enough to keep the stream and SSIM cheap.
const SAMPLE_WIDTH: u32 = 640;

/// How many lines of ffmpeg's stderr to report on failure.
const STDERR_LINES: usize = 5;

/// A child that cannot outlive the scan, on any path out of it.
struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn scaled_size(width: u32, height: u32, target: u32) -> (u32, u32) {
    let scaled = ((height as u64 * target as u64) / width.max(1) as u64) as u32;
    (target, (scaled & !1).max(2))
}

pub(crate) fn tail_lines(text: &str) -> String {
    let lines: Vec<_> = text.lines().collect();
    lines[lines.len().saturating_sub(STDERR_LINES)..].join("\n")
}

/// Returns the `(onset, capture)` of every slide.
pub(crate) fn scan(
    ffmpeg: &Path,
    video: &Path,
    info: VideoInfo,
    params: &SlideParams,
    cancelled: &(dyn Fn() -> bool + Sync),
    on_event: &mut dyn FnMut(Event),
) -> Result<Vec<(f64, f64)>, Error> {
    let interval = params.sample_interval;
    let (width, height) = scaled_size(info.width, info.height, SAMPLE_WIDTH);
    let filter = format!(
        "fps={:.6},scale={width}:{height},format=gray",
        1.0 / interval
    );
    let mut child = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-an"])
        .arg("-i")
        .arg(video)
        .args(["-vf", &filter, "-f", "rawvideo", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Error::Spawn)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut child = Kill(child);
    // Both pipes need a reader of their own: an unread stderr blocks ffmpeg once
    // full, and a read on stdout could not otherwise notice a cancel.
    let stderr = std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_end(&mut text);
        }
        String::from_utf8_lossy(&text).into_owned()
    });
    let frame_len = width as usize * height as usize;
    let (sender, frames) = sync_channel::<std::io::Result<Vec<u8>>>(1);
    std::thread::spawn(move || {
        let Some(mut stdout) = stdout else { return };
        loop {
            let mut frame = vec![0u8; frame_len];
            match stdout.read_exact(&mut frame) {
                Ok(()) => {
                    if sender.send(Ok(frame)).is_err() {
                        return;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return,
                Err(error) => {
                    let _ = sender.send(Err(error));
                    return;
                }
            }
        }
    });

    on_event(Event::ScanStarted {
        estimate: (info.duration / interval).round().max(1.0) as u64,
    });
    let mut detector = Detector::new(*params);
    let mut times = Vec::new();
    let mut sampled = 0u64;
    loop {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        let bytes = match frames.recv_timeout(POLL) {
            Ok(frame) => frame.map_err(Error::Io)?,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        // The ideal time, not the frame's own: the fps filter drifts by less than an
        // interval, and scene detection only needs to the second.
        let t = sampled as f64 * interval;
        sampled += 1;
        on_event(Event::Sampled { frames: sampled });
        let Some(frame) = GrayImage::from_raw(width, height, bytes) else {
            continue;
        };
        if let Some(found) = detector.push(t, &frame) {
            times.push(found);
            on_event(Event::CandidateFound {
                slides: times.len(),
            });
        }
    }
    times.extend(detector.finish());
    let status = child.0.wait().map_err(Error::Io)?;
    let stderr = stderr.join().unwrap_or_default();
    if !status.success() {
        return Err(Error::ScanFailed {
            status,
            stderr: tail_lines(&stderr),
        });
    }
    on_event(Event::ScanFinished {
        frames: sampled,
        slides: times.len(),
    });
    Ok(times)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_height_stays_even_and_positive() {
        assert_eq!(scaled_size(1280, 410, 640), (640, 204));
        assert_eq!(scaled_size(1280, 720, 640), (640, 360));
        assert_eq!(scaled_size(10_000, 1, 640), (640, 2));
    }

    #[test]
    fn stderr_keeps_its_last_lines() {
        assert_eq!(tail_lines("1\n2\n3\n4\n5\n6\n7"), "3\n4\n5\n6\n7");
        assert_eq!(tail_lines("only"), "only");
    }
}
