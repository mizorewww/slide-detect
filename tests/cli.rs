//! The `slide-detect` binary end to end, on videos ffmpeg generates as in
//! `synthetic.rs`. Tests that need ffmpeg are skipped without it unless `CI` is set.

#![cfg(feature = "cli")]

use std::path::Path;
use std::process::{Command, Output};

const FFMPEG: &str = "ffmpeg";
const BIN: &str = env!("CARGO_BIN_EXE_slide-detect");

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

/// Three pages: white at 0, gray from 7.5 (sampled), white again from 14.5.
fn lecture(dir: &Path) -> std::path::PathBuf {
    let video = dir.join("lecture.mp4");
    make_video(
        &video,
        20,
        &[
            ("black", 6.25, 7.25),
            ("gray", 7.25, 14.25),
            ("white", 14.25, 20.),
        ],
    );
    video
}

/// The settings `synthetic.rs` uses for the same video.
const TUNING: [&str; 6] = [
    "--sample-interval",
    "0.5",
    "--similarity",
    "0.9",
    "--cooldown",
    "10",
];

fn run(args: &[&str], video: &Path) -> Output {
    Command::new(BIN).args(args).arg(video).output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn scan_prints_the_true_times() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = lecture(dir.path());
    let tmp = dir.path().join("tmp");
    std::fs::create_dir(&tmp).unwrap();
    let output = Command::new(BIN)
        .args([&["scan", "--json"][..], &TUNING[..]].concat())
        .arg(&video)
        .env("TMPDIR", &tmp)
        .env("TEMP", &tmp)
        .env("TMP", &tmp)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let value = json(&output);
    let onsets: Vec<_> = value["slides"]
        .as_array()
        .unwrap()
        .iter()
        .map(|slide| slide["onset"].as_f64().unwrap())
        .collect();
    assert_eq!(onsets, [0.0, 7.5, 14.5]);
    assert_eq!(value["slides"][1]["capture"], 10.0);
    assert_eq!(value["slides"][1]["number"], 2);
    // Progress went to stderr, and only the JSON to stdout.
    assert!(stderr(&output).contains("Scanned 40 frames: 3 slides"));
    // No image was extracted, not even to a temporary directory.
    assert!(!stderr(&output).contains("Extract"), "{}", stderr(&output));
    assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
}

#[test]
fn scan_prints_a_table_by_default() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = lecture(dir.path());
    let output = run(&[&["scan", "--quiet"][..], &TUNING[..]].concat(), &video);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "    #  ONSET         CAPTURE\n\
         \x20   1  00:00:00.000  00:00:00.000\n\
         \x20   2  00:00:07.500  00:00:10.000\n\
         \x20   3  00:00:14.500  00:00:19.500\n"
    );
    assert_eq!(stderr(&output), "", "--quiet shows no progress");
}

#[test]
fn extract_names_each_image_by_number_and_onset() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = lecture(dir.path());
    let out = dir.path().join("slides");
    let output = run(
        &[
            &["extract", "--json", "-o", out.to_str().unwrap()][..],
            &TUNING[..],
        ]
        .concat(),
        &video,
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let expected = [
        "slide_0001_00-00-00.000.jpg",
        "slide_0002_00-00-07.500.jpg",
        "slide_0003_00-00-14.500.jpg",
    ];
    let value = json(&output);
    let files: Vec<_> = value["slides"]
        .as_array()
        .unwrap()
        .iter()
        .map(|slide| slide["file"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(files, expected);
    // The images and nothing else: the staging directory is gone.
    let mut found: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    found.sort();
    assert_eq!(found, expected);
    for name in expected {
        let bytes = std::fs::read(out.join(name)).unwrap();
        assert_eq!(&bytes[..2], [0xff, 0xd8], "a JPEG");
    }
    assert!(stderr(&output).contains("Extracted 3 images"));
}

#[test]
fn a_missing_ffmpeg_is_explained() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("video.mp4");
    std::fs::write(&video, b"").unwrap();
    let output = Command::new(BIN)
        .args(["scan", "--quiet"])
        .arg(&video)
        .env("PATH", dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("ffmpeg not found on PATH"),
        "{}",
        stderr(&output)
    );

    let missing = dir.path().join("no-such-ffmpeg");
    let output = Command::new(BIN)
        .args(["scan", "--ffmpeg"])
        .arg(&missing)
        .arg(&video)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("ffmpeg not found at"));
}

#[test]
fn bad_input_is_an_error_not_a_panic() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let missing = run(&["scan"], &dir.path().join("missing.mp4"));
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("no such file"));

    let broken = dir.path().join("broken.mp4");
    std::fs::write(&broken, b"not a video").unwrap();
    let output = run(&["scan"], &broken);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no video stream"));
    assert!(!stderr(&output).contains("panicked"));
}

#[test]
fn bad_settings_are_usage_errors() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("video.mp4");
    for args in [
        &["scan", "--similarity", "1.5"][..],
        &["scan", "--sample-interval", "0"],
        &["scan", "--cooldown", "-1"],
        &["scan", "--roi", "0,0,1"],
        &["scan", "--roi", "0.5,0,0.25,1"],
        &["extract"],
    ] {
        let output = run(args, &video);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(stderr(&output).contains("error:"), "{args:?}");
    }
}

/// A terminal's Ctrl-C reaches the whole process group, ffmpeg included.
#[cfg(unix)]
#[test]
fn ctrl_c_cancels_cleanly() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("long.mp4");
    make_video(&video, 30, &[("black", 6.25, 30.)]);
    let out = dir.path().join("slides");
    let mut child = Command::new(BIN)
        .args(["extract", "--sample-interval", "0.1", "-o"])
        .arg(&out)
        .arg(&video)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let first = lines.next().unwrap().unwrap();
    assert!(first.starts_with("Scanning"), "{first}");
    let started = Instant::now();
    let status = Command::new("kill")
        .args(["-INT", "--", &format!("-{}", child.id())])
        .status()
        .unwrap();
    assert!(status.success());
    let rest: Vec<_> = lines.map(Result::unwrap).collect();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130), "{rest:?}");
    assert_eq!(
        rest.last().map(String::as_str),
        Some("slide-detect: cancelled")
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    // No image, and no staging directory left behind.
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
}

/// A Ctrl-C while ffmpeg describes the video, before any scan, cancels too.
#[cfg(unix)]
#[test]
fn ctrl_c_during_the_probe_cancels() {
    let output = signal_during_the_probe("-INT", true);
    assert_eq!(output.status.code(), Some(130), "{}", stderr(&output));
    assert_eq!(stderr(&output), "slide-detect: cancelled\n");
}

/// SIGTERM, as a service manager sends to the process alone, exits 128 + 15.
#[cfg(unix)]
#[test]
fn sigterm_exits_143() {
    let output = signal_during_the_probe("-TERM", false);
    assert_eq!(output.status.code(), Some(143), "{}", stderr(&output));
    assert_eq!(stderr(&output), "slide-detect: terminated\n");
}

/// Sends `signal` to the tool's process group, as a terminal does, or to the tool
/// alone, while a fake ffmpeg hangs in the probe.
#[cfg(unix)]
fn signal_during_the_probe(signal: &str, group: bool) -> Output {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("video.mp4");
    std::fs::write(&video, b"").unwrap();
    // An "ffmpeg" that announces itself and hangs, as on a slow network disk.
    let started = dir.path().join("started");
    let ffmpeg = dir.path().join("ffmpeg");
    std::fs::write(
        &ffmpeg,
        format!("#!/bin/sh\ntouch '{}'\nexec sleep 30\n", started.display()),
    )
    .unwrap();
    std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
    let child = Command::new(BIN)
        .args(["scan", "--ffmpeg"])
        .arg(&ffmpeg)
        .arg(&video)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !started.exists() {
        assert!(Instant::now() < deadline, "the probe never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let target = if group {
        format!("-{}", child.id())
    } else {
        child.id().to_string()
    };
    let status = Command::new("kill")
        .args([signal, "--", &target])
        .status()
        .unwrap();
    assert!(status.success());
    child.wait_with_output().unwrap()
}

#[test]
fn a_closed_stderr_is_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(BIN)
        .args(["scan", "--ffmpeg"])
        .arg(dir.path().join("no-such-ffmpeg"))
        .arg(dir.path().join("video.mp4"))
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Nobody reads the error: writing it fails.
    drop(child.stderr.take());
    assert_eq!(child.wait().unwrap().code(), Some(1), "not 101, a panic");
}

#[test]
fn an_image_that_cannot_be_replaced_writes_none() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = lecture(dir.path());
    let out = dir.path().join("slides");
    // A directory where the second image belongs.
    std::fs::create_dir_all(out.join("slide_0002_00-00-07.500.jpg")).unwrap();
    let output = run(
        &[
            &["extract", "-q", "-o", out.to_str().unwrap()][..],
            &TUNING[..],
        ]
        .concat(),
        &video,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("a directory is in the way; no image was written"),
        "{}",
        stderr(&output)
    );
    // Not even the first image, and no staging directory.
    let names: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["slide_0002_00-00-07.500.jpg"]);
}

#[test]
fn a_video_shorter_than_the_interval_is_explained() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("short.mp4");
    make_video(&video, 1, &[("black", 0.5, 1.)]);
    let output = run(&["scan", "-q", "--sample-interval", "5"], &video);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("the video may be shorter than --sample-interval"),
        "{}",
        stderr(&output)
    );
}
