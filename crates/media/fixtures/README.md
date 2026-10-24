# Capability-check bitstreams

`spherecut doctor` embeds these (see `src/doctor.rs`) and decodes each one
in software, on VA-API and on FFmpeg's Vulkan device. Each is one 256x256
intra frame of FFmpeg's `testsrc2` pattern, 4:2:0, as a raw elementary
stream: Annex B for H.264 and HEVC (parameter sets in-band), Low Overhead
Bitstream Format OBUs for AV1 (temporal delimiter, sequence header, frame).
Doctor sends the whole file to the decoder as one packet, so no demuxer or
parser is needed and the check works on FFmpeg builds that have few of
either. 256x256 is above every hardware decoder's minimum size, and the
frames are small enough to keep in the binary (a few KB each).

| File | Format | Size |
|---|---|---|
| `h264-high.h264` | H.264 High, 8-bit | 3384 bytes |
| `hevc-main.hevc` | HEVC Main, 8-bit | 2988 bytes |
| `hevc-main10.hevc` | HEVC Main 10, 10-bit | 3005 bytes |
| `av1-main.obu` | AV1 Main (profile 0), 8-bit | 1215 bytes |

`av1-main.mp4` (1986 bytes) is the AV1 frame in MP4, for
`tests/hw_refusal.rs`, which decodes it through `VideoDecoder`. That opens
only MP4/MOV and Matroska/WebM files, not raw OBUs (DECISIONS D24).
Doctor doesn't use it.

## Regenerating

Made with Ubuntu 24.04's FFmpeg 6.1.1 (x264 0.164.3108, x265 3.5,
SVT-AV1 1.7.0), where these commands, run from this directory, reproduce
the files byte for byte. Another FFmpeg or encoder version makes
different bytes that work just as well: doctor only needs one valid frame
of each profile.

```sh
src=testsrc2=size=256x256:rate=1
ffmpeg -f lavfi -i $src -frames:v 1 -pix_fmt yuv420p \
  -c:v libx264 -profile:v high -preset veryslow -qp 34 \
  -bsf:v filter_units=remove_types=6 -f h264 h264-high.h264
ffmpeg -f lavfi -i $src -frames:v 1 -pix_fmt yuv420p \
  -c:v libx265 -profile:v main -preset veryslow \
  -x265-params qp=34:info=0:log-level=error -f hevc hevc-main.hevc
ffmpeg -f lavfi -i $src -frames:v 1 -pix_fmt yuv420p10le \
  -c:v libx265 -profile:v main10 -preset veryslow \
  -x265-params qp=34:info=0:log-level=error -f hevc hevc-main10.hevc
ffmpeg -f lavfi -i $src -frames:v 1 -pix_fmt yuv420p \
  -c:v libsvtav1 -preset 6 -crf 45 -f obu av1-main.obu
ffmpeg -i av1-main.obu -c copy -map_metadata -1 -fflags +bitexact -f mp4 av1-main.mp4
```

`filter_units=remove_types=6` drops x264's SEI with its settings string
(about 700 bytes), and x265's `info=0` does the same for x265. Check the
result with:

```sh
for f in *.h264 *.hevc *.obu *.mp4; do
  ffprobe -v error -show_entries stream=codec_name,profile,width,height,pix_fmt -of compact "$f"
done
```

which should print

```
stream|codec_name=h264|profile=High|width=256|height=256|pix_fmt=yuv420p
stream|codec_name=hevc|profile=Main|width=256|height=256|pix_fmt=yuv420p
stream|codec_name=hevc|profile=Main 10|width=256|height=256|pix_fmt=yuv420p10le
stream|codec_name=av1|profile=Main|width=256|height=256|pix_fmt=yuv420p
stream|codec_name=av1|profile=Main|width=256|height=256|pix_fmt=yuv420p
```

The Tier 1 tests in `tests/doctor.rs` decode all four in software and
check their size and pixel format.
