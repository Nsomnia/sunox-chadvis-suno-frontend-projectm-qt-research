# Song-video download and audio quality

The default `download` (`--source video`) route reuses a current song video, downloads its exact
unencrypted HTTPS URL without account credentials or redirects, and copies the first audio stream
into M4A. `--video` instead saves the original MP4. Missing/stale videos are generated automatically unless `--read-only`; `--video-timeout` controls that render's bounded wait.

```sh
# Save the existing video's original audio, without a Suno write.
sunox download CLIP_ID --source video --read-only --output ./songs

# Reuse or generate a missing/stale video before extraction.
sunox download CLIP_ID --video-timeout 180 --output ./songs

# Preserve the original video file.
sunox download CLIP_ID --source video --video --read-only --output ./videos
```

Explicit official exports and plaintext CDN audio downloads remain separate sources.
Video mode never automatically switches to either route, authorizes a download, aligns lyrics,
or performs a format/bitrate conversion. It supports M4A audio output only. Re-encoding a video
AAC track to WAV or 320 kbps MP3 cannot recover information lost before the video was created.
The official MP3/M4A/WAV formats remain available through `--source official`.

## D1 — video by default, allowance export only by explicit source

**Decision:** Ordinary downloads reuse a current video or generate a missing/stale video, then
extract unchanged M4A audio. The user explicitly requested this default. `--source official`
is required to select allowance-consuming exports; `--format wav` or a video failure cannot
select them implicitly. `--read-only` permits reuse only and blocks rendering. The old
`--generate-video` flag remains accepted for compatibility. `--video-timeout` needs no extra flag.

Local tools/output destinations are checked before rendering, the account lock is acquired only
for generation, and the clip is re-read after acquiring it so another invocation's completed video
can be reused. If clip metadata still lacks a usable video, the status endpoint is inspected:
completed current URLs are reused, running/unknown states are polled without submission, and
status errors stop the command. Only terminal absence/staleness or a confirmed failure allows
rendering. This prevents a timeout followed by another ordinary download from resubmitting an
existing job. Read-only recovery can use a status URL even when clip metadata has not caught up.
Ownership, completion, non-trash, feature and restriction checks still apply.

**Challenge:** A lost POST response must not cause a retry. Accepted: retain the existing
single-shot sender, ambiguity diagnostics, durable clip-ID journal, read-only status recovery,
and generated-resource details when a later step fails. Read-only mode may reuse existing video,
but refuses a missing/stale video that would require creation. No fallback consumes download
allowance implicitly. Confidence: high for protocol shape; exemption from future accounting is unknown.

## D2 — preserve the stream without implementing an MP4 muxer

**Dependencies:** Existing Rust download/staging/validation code (`self_owned`) handles transfer
and atomic commit. The user's locally installed FFprobe inspects codec/rate/channel metadata.
FFmpeg stream-copy performs the optional audio-only remux. Both executables are optional runtime
dependencies for this route; normal Sunox operation does not install or require them.

**Options:** (1) maintain an in-house ISO BMFF track/sample/chunk remuxer; (2) introduce a native
media library dependency in the release binary; (3) use optional local media executables.

**Decision:** Select (3). (1) adds a large correctness surface around fragmented MP4 and chunk
offsets. (2) has no verified existing cross-platform packaging in this repository. (3) is validated
on the current host and leaves the release binary's dependency set unchanged. Extraction requires
FFprobe and FFmpeg on PATH; saving MP4 requires FFprobe. Missing tools fail before a render. No
shell interprets arguments; children have a 120-second limit, bounded output and kill-on-drop.
Media tools only receive downloaded local paths, with network input protocols excluded.

**Challenge:** Reported quality is not invented. Accepted: JSON emits source/output stream codec,
profile, bit rate, sample rate, channel count and video dimensions/frame rate where supplied.
Audio extraction uses `-map 0:a:0 -vn -c:a copy`; codec/rate/channels are checked after remux and
container validation precedes atomic commit. Unknown unsupported codecs fail rather than transcode.
Confidence: high for AAC/Opus copying; revisit when packaging a native muxer or supporting new codecs.

## Quality evidence, 2026-10-15

The verified per-song POST accepts a clip ID only, no container/quality/size/bitrate selector. Status
returns one `video_url`. The current ordinary song-video path exposes one MP4 rendition; this is
not a claim about every other Suno video product, Studio export, or future endpoint.

The explicitly authorized test song produced:

| Track | Actual measured parameters |
|---|---|
| Video | MP4, H.264 High, 824 × 1464, 10 fps, approximately 100 kbps video |
| Audio in that video | AAC-LC, 191,977 bps (approximately 192 kbps), 48 kHz, stereo |
| Audio-only extraction | Same AAC packets in M4A; 920 packet hashes matched; complete decode succeeded |

These are one file's parameters, not global fixed limits. The highest fidelity available from this
video route is the original encoded audio; upsampling, raising a target bitrate, or changing file
extensions cannot improve it. FFmpeg's [streamcopy documentation](https://www.ffmpeg.org/ffmpeg.html#Streamcopy)
explains that streamcopy omits decoding and encoding.

For master/export workflows use Suno's official WAV capability where available. Suno documents
[WAV for Pro/Premier](https://help.suno.com/en/articles/2479873); it separately advertises
[32-bit/48kHz multitrack and stem exports in Studio for Premier](https://www.suno.com/blog/studio-2).
The Studio specification is not a claim that ordinary song videos contain that fidelity, or that
converting AAC to 32-bit PCM restores the original. This implementation does not add Studio export.

The first manually authorized video run left download usage at 3/20, extra remaining at 7,
generation credits unchanged and the song still locked. That is a measured observation for one
account/song; the CLI does not label this route universally free or permanently quota-exempt.
