# vidwatcher

A small daemon that watches directories and re-encodes new videos to
**AV1 video + Opus audio** in a **WebM** container — a compact, royalty-free
combination that plays natively on modern Android (AV1 decode on Android 10+,
Opus on Android 7+).

Subtitles, data streams and attachments are dropped. Every audio track is kept
and re-encoded to Opus. Stream metadata (including phone-video rotation) is kept.
A per-file state database stops the daemon from converting the same file twice.

## Install (Debian / Ubuntu)

```sh
sudo apt install ./vidwatcher_1.0.0_amd64.deb
```

This installs the `vidwatcher` binary, a `vidwatcher.service` unit (enabled but
**not** started), and the default config at `/etc/vidwatcher/config.toml`.
It pulls in `ffmpeg` as a dependency.

> The distro `ffmpeg` may only ship the slow `libaom-av1` encoder. For much
> faster encodes install an ffmpeg build with `libsvtav1` — vidwatcher picks the
> best available automatically (`libsvtav1` → `libaom-av1` → `librav1e`).

Then:

```sh
sudoedit /etc/vidwatcher/config.toml     # set `watch = [...]`
sudo systemctl start vidwatcher
journalctl -u vidwatcher -f              # watch it work
```

By default the service runs as the unprivileged `vidwatcher` user, which must be
able to read (and write, unless `output.mode = "directory"`) your media folders.
To run it as yourself instead:

```sh
sudo systemctl edit vidwatcher
# [Service]
# User=me
# Group=me
```

### Building the package

```sh
packaging/build-deb.sh        # needs cargo, dpkg-deb, fakeroot
```

## Configuration

Config is TOML, loaded from the first of:

1. `--config <FILE>`
2. `$VIDWATCHER_CONFIG`
3. `~/.config/vidwatcher/config.toml`
4. `/etc/vidwatcher/config.toml`

See [`packaging/config.toml`](packaging/config.toml) for the fully commented
default. Key settings:

| Key | Meaning | Default |
|-----|---------|---------|
| `watch` | list of directories to scan | `[]` |
| `recursive` | descend into sub-directories | `true` |
| `scan-interval` | time between passes (`"15m"`, `"2h"`, …) | `15m` |
| `min-file-age` | ignore files touched more recently than this | `60s` |
| `max-attempts` | give up on a file after N failures | `3` |
| `state-file` | processed-file database | `/var/lib/vidwatcher/state.json` |
| `encode.crf` | quality 0–63, lower = better/bigger | `32` |
| `encode.preset` | speed knob (SVT `-preset` / aom `-cpu-used` / rav1e `-speed`) | `6` |
| `encode.audio-bitrate` | Opus kbit/s per track | `128` |
| `encode.bit-depth` | `8` (max HW compat) or `10` (smaller) | `10` |
| `output.mode` | `beside` or `directory` (mirror tree) | `beside` |
| `output.directory` | target root for `mode = "directory"` | — |
| `output.suffix` | text before `.webm` in output names | `""` |
| `output.replace` | delete source after success | `false` |
| `output.skip-av1` | skip files already in AV1 | `true` |
| `output.preserve-timestamps` | copy source mtime/atime onto the output | `true` |
| `output.max-output-ratio` | discard the re-encode & keep the original if it's not smaller than `size * ratio` | `1.0` |
| `log.level` | `error`…`trace` | `info` |
| `log.file` | also append logs here | — |

Apply changes with `systemctl restart vidwatcher`.

## Running by hand

```
vidwatcher [--config FILE] [--once] [--check-config] [--log-level LEVEL]

--once            do a single scan pass and exit (good for cron)
--check-config    print the resolved configuration and exit
```

## Troubleshooting

**`loading state /var/lib/vidwatcher/state.json: Permission denied`** — you ran
`vidwatcher` as root at some point (e.g. `sudo vidwatcher --once`) and the state
file is now owned by root, unreadable by the `vidwatcher` service user:

```sh
sudo systemctl stop vidwatcher
sudo rm -f /var/lib/vidwatcher/state.json /var/lib/vidwatcher/state.json.tmp
sudo chown -R vidwatcher:vidwatcher /var/lib/vidwatcher
sudo systemctl start vidwatcher
```

**Watched/output directories under `/home`** — the unprivileged `vidwatcher`
user usually cannot traverse `/home/<you>` or write next to your files. Either
run the service as yourself:

```sh
sudo systemctl edit vidwatcher      # [Service] \n User=you \n Group=you
sudo rm -f /var/lib/vidwatcher/state.json    # was owned by the old user
sudo systemctl restart vidwatcher
```

or use `output.mode = "directory"` pointing somewhere the service user owns.

## Notes

- Outputs are written as `name.webm` (or `name.av1.webm` when the source is
  itself `name.webm`). With `output.mode = "directory"` the watched tree is
  mirrored under `output.directory`, leaving sources untouched.
- CRF picks the quality; preset picks how hard the encoder works for that
  quality. Lower preset = smaller file, slower. Try `preset = 8` on big batches
  with `libaom-av1`.
- 10-bit AV1 is actually *smaller* than 8-bit at equal quality; use `bit-depth =
  8` only if a target device can't hardware-decode 10-bit.
- Re-encoding a file that is already small or low-bitrate can produce a *larger*
  AV1 file with no quality benefit. `max-output-ratio` guards against this: if the
  result isn't small enough the output is deleted, the original is left in place,
  and the file is marked done so it isn't retried. Such files show as `kept` in
  the pass summary.
- With `preserve-timestamps` the output keeps the source's *modified* and
  *accessed* times. A file's *creation* (birth) time cannot be set on Linux — no
  syscall exists — so on the new file it will be the conversion time. The inode
  *change* time (`ctime`) likewise always reflects the last metadata change.
