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
    estimate: u64,
    found: usize,
    drawn: Option<Instant>,
}

impl Progress {
    pub fn new(quiet: bool) -> Self {
        let mode = if quiet {
            Mode::Quiet
        } else if std::io::stderr().is_terminal() {
            Mode::Live
        } else {
            Mode::Lines
        };
        Self {
            mode,
            estimate: 0,
            found: 0,
            drawn: None,
        }
    }

    pub fn event(&mut self, event: Event) {
        match event {
            Event::ScanStarted { estimate } => {
                self.estimate = estimate;
                self.line(&format!("Scanning: about {estimate} frames to sample"));
            }
            Event::Sampled { frames } => {
                let percent = (frames * 100 / self.estimate.max(1)).min(100);
                let found = self.found;
                self.live(&format!(
                    "Scanning: {percent:>3}%  {frames}/{} frames, {found} {} so far",
                    self.estimate,
                    plural(found, "slide")
                ));
            }
            Event::CandidateFound { slides } => self.found = slides,
            Event::ScanFinished { frames, slides } => {
                self.line(&format!(
                    "Scanned {frames} frames: {slides} {}",
                    plural(slides, "slide")
                ));
            }
            Event::ExtractStarted { slides } => {
                self.line(&format!("Extracting {slides} {}", plural(slides, "image")));
            }
            Event::Extracted { done, slides } => {
                self.live(&format!("Extracting: {done}/{slides}"));
                if done == slides {
                    self.line(&format!("Extracted {slides} {}", plural(slides, "image")));
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

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}
