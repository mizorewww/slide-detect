//! The decision: given sampled pictures in order, when does a slide begin?

use crate::{Roi, SlideParams};
use image::GrayImage;
use image_compare::Algorithm;
use std::borrow::Cow;

/// Three states: the last slide, the candidate under observation, and the time of
/// each. A cooldown only limits emitting, so detection has no blind spot.
pub(crate) struct Detector {
    params: SlideParams,
    last_emitted: Option<GrayImage>,
    candidate: Option<GrayImage>,
    candidate_first: Option<f64>,
    candidate_last: Option<f64>,
    last_emit: f64,
}

impl Detector {
    pub fn new(params: SlideParams) -> Self {
        Self {
            params,
            last_emitted: None,
            candidate: None,
            candidate_first: None,
            candidate_last: None,
            last_emit: f64::NEG_INFINITY,
        }
    }

    /// Takes the frame sampled at `t`; returns `(onset, capture)` when a slide is due.
    pub fn push(&mut self, t: f64, frame: &GrayImage) -> Option<(f64, f64)> {
        let compared = crop_roi(frame, self.params.roi);
        let similarity = self.params.similarity;
        let differs = |image: &GrayImage| {
            image.dimensions() != compared.dimensions() || ssim(image, &compared) < similarity
        };
        if !self.last_emitted.as_ref().is_none_or(differs) {
            // Back to the last slide: the candidate was a transition. Drop it.
            self.candidate = None;
            self.candidate_first = None;
            return None;
        }
        // A candidate that itself changed means an animation: restart its clock.
        if self.candidate.as_ref().is_none_or(differs) {
            self.candidate = Some(compared.into_owned());
            self.candidate_first = Some(t);
        }
        self.candidate_last = Some(t);
        let onset = self.candidate_first.unwrap_or(t);
        let capture = self.candidate_last.unwrap_or(t);
        if t - onset >= self.params.stable_secs && t - self.last_emit >= self.params.cooldown {
            self.last_emitted = self.candidate.take();
            self.candidate_first = None;
            self.candidate_last = None;
            self.last_emit = t;
            return Some((onset, capture));
        }
        None
    }

    /// At the end of the video a settled candidate is a slide even within a
    /// cooldown; otherwise the last slide would always be lost.
    pub fn finish(self) -> Option<(f64, f64)> {
        self.candidate.as_ref()?;
        let (onset, capture) = (self.candidate_first?, self.candidate_last?);
        (capture - onset >= self.params.stable_secs).then_some((onset, capture))
    }
}

/// Without a region the frame is borrowed, so most frames copy nothing.
fn crop_roi(image: &GrayImage, roi: Option<Roi>) -> Cow<'_, GrayImage> {
    let Some(roi) = roi else {
        return Cow::Borrowed(image);
    };
    let (w, h) = image.dimensions();
    let (x1, y1, x2, y2) = roi.pixels(w, h);
    Cow::Owned(
        image::imageops::crop_imm(image, x1, y1, (x2 - x1).max(1), (y2 - y1).max(1)).to_image(),
    )
}

impl Roi {
    /// The region in pixels of a `w`×`h` frame: `(x1, y1, x2, y2)`, never empty.
    pub fn pixels(&self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let sx = |v: f64| (v * w as f64).round() as u32;
        let sy = |v: f64| (v * h as f64).round() as u32;
        let (x1, x2) = (
            sx(self.x1).min(w.saturating_sub(1)),
            sx(self.x2).clamp(1, w),
        );
        let (y1, y2) = (
            sy(self.y1).min(h.saturating_sub(1)),
            sy(self.y2).clamp(1, h),
        );
        (x1, y1, x2.max(x1 + 1), y2.max(y1 + 1))
    }
}

fn ssim(a: &GrayImage, b: &GrayImage) -> f64 {
    image_compare::gray_similarity_structure(&Algorithm::MSSIMSimple, a, b)
        .map(|s| s.score)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame with distinct structure per `seed`, as SSIM needs to tell them apart.
    fn frame(seed: u8) -> GrayImage {
        GrayImage::from_fn(64, 64, |x, y| {
            let band = ((x / 8) as u8).wrapping_mul(seed) ^ ((y / 8) as u8).wrapping_mul(seed / 2);
            image::Luma([band.wrapping_mul(37)])
        })
    }

    fn params(cooldown: f64, stable_secs: f64) -> SlideParams {
        SlideParams {
            sample_interval: 0.5,
            similarity: 0.9,
            cooldown,
            stable_secs,
            roi: None,
        }
    }

    /// Runs samples 0.5 s apart; returns every `(onset, capture)` and the flush.
    fn run(params: SlideParams, seeds: &[u8]) -> Vec<(f64, f64)> {
        let mut detector = Detector::new(params);
        let mut out: Vec<_> = seeds
            .iter()
            .enumerate()
            .filter_map(|(i, seed)| detector.push(i as f64 * 0.5, &frame(*seed)))
            .collect();
        out.extend(detector.finish());
        out
    }

    #[test]
    fn a_change_is_a_slide_and_a_repeat_is_not() {
        let found = run(params(0.0, 0.0), &[1, 1, 1, 9, 9, 9]);
        assert_eq!(found, [(0.0, 0.0), (1.5, 1.5)]);
    }

    #[test]
    fn a_slide_during_a_cooldown_keeps_its_true_time() {
        // Page 9 appears at 1.0 s; the cooldown ends at 2.0 s.
        let found = run(params(2.0, 0.0), &[1, 1, 9, 9, 9, 9, 9]);
        assert_eq!(found, [(0.0, 0.0), (1.0, 2.0)]);
    }

    #[test]
    fn the_last_page_survives_a_cooldown() {
        let found = run(params(100.0, 0.0), &[1, 1, 9, 9]);
        assert_eq!(found, [(0.0, 0.0), (1.0, 1.5)]);
    }

    #[test]
    fn stable_mode_skips_a_passing_transition() {
        // 5 shows for 0.5 s, then 9 settles.
        let found = run(params(0.0, 1.0), &[1, 1, 1, 5, 5, 9, 9, 9, 9]);
        assert_eq!(found, [(0.0, 1.0), (2.5, 3.5)]);
    }

    #[test]
    fn returning_to_the_last_slide_drops_the_candidate() {
        let found = run(params(100.0, 0.0), &[1, 1, 9, 1, 1]);
        assert_eq!(found, [(0.0, 0.0)]);
    }

    #[test]
    fn changes_outside_the_region_are_ignored() {
        let roi = Some(Roi {
            x1: 0.0,
            y1: 0.0,
            x2: 0.5,
            y2: 1.0,
        });
        let mut detector = Detector::new(SlideParams {
            roi,
            ..params(0.0, 0.0)
        });
        let base = frame(1);
        let mut changed = base.clone();
        for (x, _, pixel) in changed.enumerate_pixels_mut() {
            if x >= 40 {
                *pixel = image::Luma([255]);
            }
        }
        assert!(detector.push(0.0, &base).is_some());
        assert!(detector.push(0.5, &changed).is_none());
    }

    #[test]
    fn regions_are_never_empty() {
        let roi = Roi {
            x1: 1.0,
            y1: 1.0,
            x2: 1.0,
            y2: 1.0,
        };
        assert_eq!(roi.pixels(10, 10), (9, 9, 10, 10));
    }
}
