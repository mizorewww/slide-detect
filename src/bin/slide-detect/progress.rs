//! The library's events, as progress on stderr: a live line at a terminal, a line
//! per phase otherwise, nothing with `--quiet`.

use slide_detect::Event;
use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

/// Redraws of the live line, at most this often.
const REDRAW: Duration = Duration::from_millis(100);

#[derive(PartialEq)]
enum Mode {
    Quiet,
    Lines,
    Live,
}

pub struct Progress {
    mode: Mode,
    /// None when the duration is unknown, and the library's estimate meaningless.
    estimate: Option<u64>,
    known_duration: bool,
    found: usize,
    drawn: Option<Instant>,
}

impl Progress {
    pub fn new(quiet: bool, known_duration: bool) -> Self {
        let mode = if quiet {
            Mode::Quiet
        } else if std::io::stderr().is_terminal() {
            Mode::Live
        } else {
            Mode::Lines
        };
        Self {
            mode,
            estimate: None,
            known_duration,
            found: 0,
            drawn: None,
        }
    }

    pub fn event(&mut self, event: Event) {
        match event {
            Event::ScanStarted { estimate } => {
                self.estimate = self.known_duration.then_some(estimate);
                self.line(&scan_started(self.estimate));
            }
            Event::Sampled { frames } => {
                let text = sampled(frames, self.estimate, self.found);
                self.live(&text);
            }
            Event::CandidateFound { slides } => self.found = slides,
            Event::ScanFinished { frames, slides } => {
                self.line(&format!(
                    "Scanned {frames} {}: {slides} {}",
                    plural(frames, "frame"),
                    plural(slides as u64, "slide")
                ));
            }
            Event::ExtractStarted { slides } => {
                self.line(&format!(
                    "Extracting {slides} {}",
                    plural(slides as u64, "image")
                ));
            }
            Event::Extracted { done, slides } => {
                self.live(&format!("Extracting: {done}/{slides}"));
                if done == slides {
                    self.line(&format!(
                        "Extracted {slides} {}",
                        plural(slides as u64, "image")
                    ));
                }
            }
        }
    }

    /// Clears a live line left by a run that stopped.
    pub fn finish(&mut self) {
        self.clear();
    }

    fn line(&mut self, text: &str) {
        if self.mode == Mode::Quiet {
            return;
        }
        self.clear();
        say(text);
    }

    fn live(&mut self, text: &str) {
        if self.mode != Mode::Live || self.drawn.is_some_and(|at| at.elapsed() < REDRAW) {
            return;
        }
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "\r\x1b[2K{text}");
        let _ = stderr.flush();
        self.drawn = Some(Instant::now());
    }

    fn clear(&mut self) {
        if self.drawn.take().is_some() {
            let mut stderr = std::io::stderr().lock();
            let _ = write!(stderr, "\r\x1b[2K");
            let _ = stderr.flush();
        }
    }
}

/// A line on stderr. Unlike `eprintln!`, a closed stderr is not a panic: there is
/// nobody left to tell.
pub fn say(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

fn scan_started(estimate: Option<u64>) -> String {
    match estimate {
        Some(estimate) => format!(
            "Scanning: about {estimate} {} to sample",
            plural(estimate, "frame")
        ),
        None => "Scanning: duration unknown".to_owned(),
    }
}

fn sampled(frames: u64, estimate: Option<u64>, found: usize) -> String {
    let found = format!("{found} {} so far", plural(found as u64, "slide"));
    match estimate {
        Some(estimate) => {
            let percent = (frames * 100 / estimate.max(1)).min(100);
            format!("Scanning: {percent:>3}%  {frames}/{estimate} frames, {found}")
        }
        None => format!("Scanning: {frames} {}, {found}", plural(frames, "frame")),
    }
}

fn plural(count: u64, word: &str) -> String {
    if count == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_agree_with_their_nouns() {
        assert_eq!(scan_started(Some(1)), "Scanning: about 1 frame to sample");
        assert_eq!(
            scan_started(Some(40)),
            "Scanning: about 40 frames to sample"
        );
        assert_eq!(
            sampled(1, Some(40), 1),
            "Scanning:   2%  1/40 frames, 1 slide so far"
        );
    }

    #[test]
    fn an_unknown_duration_shows_no_estimate() {
        assert_eq!(scan_started(None), "Scanning: duration unknown");
        assert_eq!(sampled(1, None, 0), "Scanning: 1 frame, 0 slides so far");
        assert_eq!(sampled(7, None, 2), "Scanning: 7 frames, 2 slides so far");
    }
}
