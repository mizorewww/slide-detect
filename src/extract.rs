//! The second pass: one full-resolution JPEG per slide.

use crate::scan::tail_lines;
use crate::{Error, Event, ExtractError, POLL, Slide, SlideTime};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

/// One ffmpeg per image: four use the cores without a storm of processes.
const CONCURRENCY: usize = 4;

const CHILD_POLL: Duration = Duration::from_millis(20);

fn file_name(number: usize) -> String {
    format!("slide_{number:04}.jpg")
}

pub(crate) fn extract(
    ffmpeg: &Path,
    video: &Path,
    times: &[SlideTime],
    output_dir: &Path,
    cancelled: &(dyn Fn() -> bool + Sync),
    on_event: &mut dyn FnMut(Event),
) -> Result<Vec<Slide>, Error> {
    let slides = times.len();
    on_event(Event::ExtractStarted { slides });
    let next = AtomicUsize::new(0);
    // Set on the way out, whatever the reason, so running children are killed.
    let stop = AtomicBool::new(false);
    let (sender, results) = channel::<(usize, Result<(), ExtractError>)>();
    std::thread::scope(|scope| {
        for _ in 0..CONCURRENCY.min(slides) {
            let sender = sender.clone();
            let (next, stop) = (&next, &stop);
            scope.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(time) = times.get(index) else {
                        return;
                    };
                    let dest = output_dir.join(file_name(index + 1));
                    let result = extract_frame(ffmpeg, video, time.capture, &dest, stop);
                    if sender.send((index, result)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(sender);
        let outcome = wait(&results, times, cancelled, on_event);
        stop.store(true, Ordering::Relaxed);
        outcome
    })?;
    Ok(times
        .iter()
        .enumerate()
        .map(|(index, time)| Slide {
            onset: time.onset,
            capture: time.capture,
            file_name: file_name(index + 1),
        })
        .collect())
}

fn wait(
    results: &std::sync::mpsc::Receiver<(usize, Result<(), ExtractError>)>,
    times: &[SlideTime],
    cancelled: &(dyn Fn() -> bool + Sync),
    on_event: &mut dyn FnMut(Event),
) -> Result<(), Error> {
    let mut done = 0;
    while done < times.len() {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        match results.recv_timeout(POLL) {
            Ok((_, Ok(()))) => {
                done += 1;
                on_event(Event::Extracted {
                    done,
                    slides: times.len(),
                });
            }
            Ok((index, Err(source))) => {
                return Err(Error::Extract {
                    number: index + 1,
                    onset: times[index].onset,
                    source,
                });
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

/// One exact frame. A stopped extraction reports no error of its own: the caller is
/// already leaving, for a reason it knows.
fn extract_frame(
    ffmpeg: &Path,
    video: &Path,
    t: f64,
    dest: &Path,
    stop: &AtomicBool,
) -> Result<(), ExtractError> {
    let seek = format!("{t:.3}");
    // -q:v 2 is JPEG's high-quality end (2 to 31, lower is better): slides are text.
    let mut child = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &seek])
        .arg("-i")
        .arg(video)
        .args(["-frames:v", "1", "-q:v", "2"])
        .arg(dest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ExtractError::Spawn)?;
    let stderr = child.stderr.take();
    let stderr = std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_end(&mut text);
        }
        String::from_utf8_lossy(&text).into_owned()
    });
    let status = loop {
        if stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(CHILD_POLL),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ExtractError::Spawn(error));
            }
        }
    };
    if !status.success() {
        return Err(ExtractError::Failed {
            code: status.code(),
            stderr: tail_lines(&stderr.join().unwrap_or_default()),
        });
    }
    if dest.is_file() {
        Ok(())
    } else {
        Err(ExtractError::Missing(dest.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_numbered_and_sort_in_time_order() {
        assert_eq!(file_name(1), "slide_0001.jpg");
        assert_eq!(file_name(12), "slide_0012.jpg");
    }
}
