//! End to end on a video ffmpeg generates: page changes between samples, so what
//! each sample sees is certain. Needs ffmpeg; skipped without it unless `CI` is set.

use slide_detect::{Error, Event, ExtractError, Slide, SlideParams, VideoInfo, detect};
use std::path::Path;
use std::process::Command;

const FFMPEG: &str = "ffmpeg";

fn ffmpeg_available() -> bool {
    let available = Command::new(FFMPEG).arg("-version").output().is_ok();
    assert!(
        available || std::env::var_os("CI").is_none(),
        "CI must install ffmpeg"
    );
    available
}

/// A white 1280x720, 10 fps video with pages filled as `(color, from, to)`.
fn make_video(path: &Path, secs: u32, pages: &[(&str, f64, f64)]) {
    let filter = pages
        .iter()
        .map(|(color, from, to)| {
            format!("drawbox=color={color}:t=fill:enable='between(t,{from},{to})'")
        })
        .collect::<Vec<_>>()
        .join(",");
    let status = Command::new(FFMPEG)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
        .args(["-i", &format!("color=c=white:s=1280x720:d={secs}:r=10")])
        .args(["-vf", &filter])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
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

fn info(secs: u32) -> VideoInfo {
    VideoInfo {
        width: 1280,
        height: 720,
        duration: secs as f64,
    }
}

fn run(video: &Path, secs: u32, params: SlideParams, out: &Path) -> (Vec<Slide>, Vec<Event>) {
    let mut events = Vec::new();
    let slides = detect(
        Path::new(FFMPEG),
        video,
        info(secs),
        &params,
        out,
        &|| false,
        &mut |event| events.push(event),
    )
    .unwrap();
    (slides, events)
}

#[test]
fn slides_keep_their_true_times_through_a_cooldown() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("synthetic.mp4");
    make_video(
        &video,
        20,
        &[
            ("black", 6.25, 7.25),
            ("gray", 7.25, 14.25),
            ("white", 14.25, 20.),
        ],
    );
    let out = dir.path().join("frames");
    let (slides, events) = run(&video, 20, params(10.0, 0.0), &out);
    let onsets: Vec<_> = slides.iter().map(|slide| slide.onset).collect();
    // White at 0. Black is replaced by gray at 7.5, which is emitted when the cooldown
    // ends with its first time. The last page is flushed at the end of the video.
    assert_eq!(onsets, [0.0, 7.5, 14.5]);
    for slide in &slides {
        let bytes = std::fs::read(out.join(&slide.file_name)).unwrap();
        assert_eq!(&bytes[..2], [0xff, 0xd8], "a JPEG");
    }
    assert_eq!(slides[1].file_name, "slide_0002.jpg");
    assert!(matches!(events[0], Event::ScanStarted { estimate: 40 }));
    assert!(events.contains(&Event::ExtractStarted { slides: 3 }));
    assert_eq!(
        events.last(),
        Some(&Event::Extracted { done: 3, slides: 3 })
    );
}

#[test]
fn stable_mode_waits_for_the_settled_picture() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("synthetic.mp4");
    make_video(&video, 10, &[("black", 2.25, 3.75), ("gray", 3.75, 10.)]);
    let (slides, _) = run(&video, 10, params(2.0, 1.2), &dir.path().join("frames"));
    let onsets: Vec<_> = slides.iter().map(|slide| slide.onset).collect();
    // Black is sampled for 1.0 s, short of 1.2: only white and gray count.
    assert_eq!(onsets, [0.0, 4.0]);
}

#[test]
fn a_set_cancel_flag_stops_the_scan() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("synthetic.mp4");
    make_video(&video, 2, &[("black", 0.75, 2.)]);
    let result = detect(
        Path::new(FFMPEG),
        &video,
        info(2),
        &params(0.0, 0.0),
        &dir.path().join("frames"),
        &|| true,
        &mut |_| {},
    );
    assert!(matches!(result, Err(Error::Cancelled)));
}

#[test]
fn an_unreadable_video_reports_ffmpegs_words() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("broken.mp4");
    std::fs::write(&video, b"not a video").unwrap();
    let result = detect(
        Path::new(FFMPEG),
        &video,
        info(2),
        &params(0.0, 0.0),
        &dir.path().join("frames"),
        &|| false,
        &mut |_| {},
    );
    match result {
        Err(Error::ScanFailed { stderr, .. }) => assert!(!stderr.is_empty()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_missing_ffmpeg_is_a_spawn_error() {
    let dir = tempfile::tempdir().unwrap();
    let result = detect(
        &dir.path().join("no-such-ffmpeg"),
        &dir.path().join("video.mp4"),
        info(2),
        &params(0.0, 0.0),
        &dir.path().join("frames"),
        &|| false,
        &mut |_| {},
    );
    assert!(matches!(result, Err(Error::Spawn(_))));
}

#[test]
fn a_failed_image_names_its_slide() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("synthetic.mp4");
    make_video(&video, 2, &[("black", 0.75, 2.)]);
    let out = dir.path().join("frames");
    // A directory where the first image belongs.
    std::fs::create_dir_all(out.join("slide_0001.jpg")).unwrap();
    let result = detect(
        Path::new(FFMPEG),
        &video,
        info(2),
        &params(0.0, 0.0),
        &out,
        &|| false,
        &mut |_| {},
    );
    match result {
        Err(Error::Extract {
            number: 1,
            onset,
            source: ExtractError::Failed { .. },
        }) => assert_eq!(onset, 0.0),
        other => panic!("{other:?}"),
    }
}
