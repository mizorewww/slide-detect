# slide-detect

Finds the slide changes in a lecture video and extracts a full-resolution JPEG of
each. ffmpeg decodes a small grayscale stream; SSIM decides when the picture changed.

- **True times**: a cooldown only limits how often slides are emitted, so a slide's
  time is when it first appeared. Detection has no blind spot.
- **Settled pictures**: `stable_secs` skips animations and transitions that never hold.
- **Regions**: `roi` compares part of the frame, ignoring a webcam or a ticker.
- **No surprises**: you pass the ffmpeg path and the video's size, and receive typed
  events and errors. The crate has no user-facing text and starts nothing else.

You need [ffmpeg](https://ffmpeg.org) at run time.

```toml
[dependencies]
slide-detect = "0.1"
```

```rust,no_run
use slide_detect::{Event, SlideParams, VideoInfo, detect};
use std::path::Path;

let params = SlideParams {
    sample_interval: 1.0,
    similarity: 0.85,
    cooldown: 5.0,
    stable_secs: 0.0,
    roi: None,
};
let info = VideoInfo { width: 1920, height: 1080, duration: 3600.0 };
let slides = detect(
    Path::new("ffmpeg"),
    Path::new("lecture.mp4"),
    info,
    &params,
    Path::new("frames"),
    &|| false,
    &mut |event| {
        if let Event::Extracted { done, slides } = event {
            println!("{done}/{slides}");
        }
    },
)?;
for slide in slides {
    println!("{:.1}s frames/{}", slide.onset, slide.file_name);
}
# Ok::<(), slide_detect::Error>(())
```

`scan` takes the same arguments except the output directory, and returns each
slide's `onset` and `capture` without extracting any image; `detect` is `scan`
followed by the extraction.

Both block; run them on their own thread in an async program. They stop all ffmpeg
processes within about a tenth of a second of `cancelled` returning true.

## Command-line tool

The crate also builds `slide-detect`, a command-line tool over the same library,
behind the `cli` feature. Library users do not get its dependencies.

```sh
cargo install --locked slide-detect --features cli
```

It needs ffmpeg on `PATH`, or its path given with `--ffmpeg`.

```console
$ slide-detect scan lecture.mp4
Scanning: about 3600 frames to sample
Scanned 3600 frames: 42 slides
    #  ONSET         CAPTURE
    1  00:00:00.000  00:00:00.000
    2  00:01:07.000  00:01:12.000
  ...

$ slide-detect extract lecture.mp4 -o slides
...
    #  ONSET         CAPTURE       FILE
    1  00:00:00.000  00:00:00.000  slide_0001_00-00-00.000.jpg
    2  00:01:07.000  00:01:12.000  slide_0002_00-01-07.000.jpg
  ...
```

- `scan <VIDEO>` prints when each slide first appears (`ONSET`) and when its image
  would be taken (`CAPTURE`), and writes no image.
- `extract <VIDEO> -o <DIR>` writes a full-resolution JPEG of each slide, named by
  number and onset, replacing files of the same name. Images are extracted into a
  hidden directory inside `<DIR>` and moved into place only once all of them exist
  and every name is known to be replaceable, so a failed or cancelled run leaves
  `<DIR>` as it was.
- `--json` prints JSON instead of a table: `video`, (for `extract`) `output`, and
  `slides`, each with `number`, `onset` and `capture` in seconds and (for `extract`)
  `file`.
- `--sample-interval`, `--similarity`, `--cooldown`, `--stable-secs` and
  `--roi X1,Y1,X2,Y2` set the fields of `SlideParams`; the defaults are 1, 0.85, 5,
  0 and the whole frame.
- Progress goes to stderr; `--quiet` hides it. Ctrl-C or SIGTERM stops every ffmpeg
  and exits.
- Exit codes: 0 on success, 1 on failure, 2 on a usage error, 130 on Ctrl-C (SIGINT)
  and 143 on SIGTERM.

## License

[MIT](LICENSE)
