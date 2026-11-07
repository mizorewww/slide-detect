//! What goes to stdout: a table for people, or JSON for programs.

use serde_json::json;
use slide_detect::Slide;
use std::fmt::Write;
use std::path::Path;

/// Milliseconds, rounded; detection times are multiples of the sample interval.
fn millis(secs: f64) -> u64 {
    (secs.max(0.0) * 1000.0).round() as u64
}

fn split(secs: f64) -> (u64, u64, u64, u64) {
    let ms = millis(secs);
    (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// `01:02:07.500`
pub fn timestamp(secs: f64) -> String {
    let (h, m, s, ms) = split(secs);
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

/// `slide_0002_01-02-07.500.jpg`: sorts in time order, and is a valid file name on
/// every system.
pub fn image_name(number: usize, onset: f64) -> String {
    let (h, m, s, ms) = split(onset);
    format!("slide_{number:04}_{h:02}-{m:02}-{s:02}.{ms:03}.jpg")
}

fn seconds(secs: f64) -> f64 {
    millis(secs) as f64 / 1000.0
}

pub fn scan_table(slides: &[Slide]) -> String {
    let mut out = format!("{:>5}  {:<12}  CAPTURE\n", "#", "ONSET");
    for (index, slide) in slides.iter().enumerate() {
        let _ = writeln!(
            out,
            "{:>5}  {}  {}",
            index + 1,
            timestamp(slide.onset),
            timestamp(slide.capture)
        );
    }
    out
}

pub fn extract_table(slides: &[Slide], files: &[String]) -> String {
    let mut out = format!("{:>5}  {:<12}  {:<12}  FILE\n", "#", "ONSET", "CAPTURE");
    for (index, (slide, file)) in slides.iter().zip(files).enumerate() {
        let _ = writeln!(
            out,
            "{:>5}  {}  {}  {file}",
            index + 1,
            timestamp(slide.onset),
            timestamp(slide.capture)
        );
    }
    out
}

pub fn scan_json(video: &Path, slides: &[Slide]) -> String {
    let slides: Vec<_> = slides
        .iter()
        .enumerate()
        .map(|(index, slide)| {
            json!({
                "number": index + 1,
                "onset": seconds(slide.onset),
                "capture": seconds(slide.capture),
            })
        })
        .collect();
    pretty(json!({ "video": video.to_string_lossy(), "slides": slides }))
}

pub fn extract_json(video: &Path, dir: &Path, slides: &[Slide], files: &[String]) -> String {
    let slides: Vec<_> = slides
        .iter()
        .zip(files)
        .enumerate()
        .map(|(index, (slide, file))| {
            json!({
                "number": index + 1,
                "onset": seconds(slide.onset),
                "capture": seconds(slide.capture),
                "file": file,
            })
        })
        .collect();
    pretty(json!({
        "video": video.to_string_lossy(),
        "output": dir.to_string_lossy(),
        "slides": slides,
    }))
}

fn pretty(value: serde_json::Value) -> String {
    let mut text = serde_json::to_string_pretty(&value).unwrap_or_default();
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_as_a_clock() {
        assert_eq!(timestamp(0.0), "00:00:00.000");
        assert_eq!(timestamp(3727.5), "01:02:07.500");
        assert_eq!(timestamp(0.1 + 0.2), "00:00:00.300");
    }

    #[test]
    fn image_names_sort_in_time_order() {
        assert_eq!(image_name(2, 67.5), "slide_0002_00-01-07.500.jpg");
        assert_eq!(image_name(12, 36_000.0), "slide_0012_10-00-00.000.jpg");
    }

    #[test]
    fn json_lists_every_slide() {
        let slides = [Slide {
            onset: 7.5,
            capture: 10.0,
            file_name: "slide_0001.jpg".into(),
        }];
        let value: serde_json::Value =
            serde_json::from_str(&scan_json(Path::new("a \"b\".mp4"), &slides)).unwrap();
        assert_eq!(value["video"], "a \"b\".mp4");
        assert_eq!(value["slides"][0]["onset"], 7.5);
        assert_eq!(value["slides"][0]["capture"], 10.0);
    }
}
