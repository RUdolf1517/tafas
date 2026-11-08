# X

Hosts: x.com, twitter.com, t.co. Detection and the host rules are in
[extractors §1](../extractors.md#1-detection).

Related: [extractors](../extractors.md) · [cli](../cli.md) · [guarantees](../guarantees.md)

This page is the part of [extractors](../extractors.md) that is about X:
its routes, what is selected, and every refusal, with the sentence it produces.
The overview keeps what applies to every site — detection, what a refusal looks
like when it happens, what happens when a site changes, and the
`--extractor-args` escape hatch — so this page can be read on its own.

## 1. Route

The syndication endpoint that the embedded-tweet widget uses:
`cdn.syndication.twimg.com/tweet-result?id=<id>&lang=en`. It returns the post as
JSON: `text`, `user.screen_name`, and `mediaDetails[]` with `video_info.variants`
for video and `media_url_https` for photos.

Variants whose `content_type` is `video/mp4` are candidates; the highest
`bit_rate` wins. An animated GIF arrives as an MP4 variant with no audio track
and is written as `.mp4`, which is what it is.

A variant whose `content_type` is `application/x-mpegURL` is an HLS playlist.
When a post offers nothing else, the playlist is walked, the segments are
demuxed and the result is written as an MP4 in the same process — no transcoding
and no external binary, exactly like the remux path. Segments encrypted with
`AES-128` are decrypted on the way in, with the key fetched once. A progressive MP4 is still
preferred when both shapes exist: one request instead of one per segment.

## 2. Photos

A post with photos and no video downloads the photos, one file each, named
`<stem>-1.jpg`, `<stem>-2.jpg` … A post with video *and* photos downloads the
video; use `--list-formats` to see both.

## 3. Refused

| Situation | Output |
| --- | --- |
| only a fragmented-MP4 playlist (`EXT-X-MAP`) | read, since v0.3: the header is fetched once and the fragments are stitched ([hls §2](hls.md#2-two-containers-one-writer)) |
| a `SAMPLE-AES` playlist | refusal at parse time: the cipher is inside the container's framing |
| tombstone (deleted, protected, age-restricted) | refusal naming the missing account |
| post with no media at all | `unsupported` |
| id not found by syndication | `unsupported`, because the endpoint answers 404 for old and private posts alike |
| `--write-subs` | a note: X carries no caption tracks |

The X refusal table stays three rows long on purpose: a post either has media or
it does not, and a caption flag cannot change that.
