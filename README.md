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
slide-detect = { git = "https://github.com/mizorewww/slide-detect" }
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

`detect` blocks; run it on its own thread in an async program. It stops all ffmpeg
processes within about a tenth of a second of `cancelled` returning true.

Not on crates.io yet; depend on it by Git revision.

## License

[MIT](LICENSE)
