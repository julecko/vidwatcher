# vidwatcher

Recursively re-encodes every video in a directory to **AV1 video + Opus audio**,
muxed into **WebM** — a compact, royalty-free combination that plays natively on
modern Android (AV1 decode on Android 10+, Opus on Android 5+/7+).

Subtitles, data streams and attachments are dropped. Every audio track is kept and
re-encoded to Opus. Stream metadata (including phone-video rotation) is preserved.

## Requirements

`ffmpeg` and `ffprobe` on `PATH`, built with an AV1 encoder. The tool auto-selects
the best available: `libsvtav1` → `libaom-av1` → `librav1e`.

## Usage

```
vidwatcher <DIR> [OPTIONS]

--crf <0-63>          quality, lower = better/bigger        [default: 32]
--preset <N>          encoder speed knob (higher = faster)  [default: 6]
--audio-bitrate <k>   Opus kbit/s per track                 [default: 128]
--bit-depth <8|10>    10 = smaller; 8 = max HW compat        [default: 10]
--output-dir <DIR>    mirror the tree here instead of writing beside sources
--suffix <TEXT>       inserted before ".webm" in output names
--skip-av1            leave files that are already AV1 alone
--overwrite           re-encode even if the output exists
--replace             delete each source after it converts
--dry-run             print planned actions only
```

Example:

```
vidwatcher ~/Videos --crf 30 --skip-av1
```

## Notes

- Outputs are written next to each source as `name.webm` (or `name.av1.webm` when
  the source is itself `name.webm`). Use `--output-dir` or `--skip-av1` to avoid
  re-processing outputs on a second run.
- `libaom-av1` is slow; raise `--preset` (e.g. `8`) for faster encodes.
