# TikTok

Hosts: tiktok.com, vm.tiktok.com, vt.tiktok.com. Detection and the host rules are in
[extractors §1](../extractors.md#1-detection).

Related: [extractors](../extractors.md) · [cli](../cli.md) · [guarantees](../guarantees.md)

This page is the part of [extractors](../extractors.md) that is about TikTok:
its routes, what is selected, and every refusal, with the sentence it produces.
The overview keeps what applies to every site — detection, what a refusal looks
like when it happens, what happens when a site changes, and the
`--extractor-args` escape hatch — so this page can be read on its own.

## 1. Routes

1. **The page.** `GET /@user/video/<id>` and the JSON inlined as
   `__UNIVERSAL_DATA_FOR_REHYDRATION__`, read at
   `__DEFAULT_SCOPE__["webapp.video-detail"].itemInfo.itemStruct`.
2. **The mobile feed.** `api22-normal-c-useast2a.tiktokv.com/aweme/v1/feed/`
   with the app user agent, read at `aweme_list[0]`. Used when the page comes
   back as a verification wall or without inline state.

Short links (`vm.tiktok.com`, `vt.tiktok.com`) are resolved once, and the final
URL supplies the id. `/photo/<id>` posts are images, not video.

## 2. Watermark

Both the page and the feed hand out several URLs for the same video. `playAddr`
is preferred, and `downloadAddr` is kept as a fallback, because the first is the
stream the player shows and the second is often the version with a burned-in
attribution. Neither is re-encoded: whatever TikTok serves is what is written.

## 3. Refused

| Situation | Output |
| --- | --- |
| verification wall on both routes | refusal naming both routes' failures |
| private or deleted post | `unsupported` with the site's status message |
| region-restricted post | refusal quoting the status message |
| photo post | images are downloaded; a photo post with no video is not an error |

TikTok names no caption tracks, so `--write-subs` there is a note rather than a
file, and `--sub-langs` names a language the site cannot offer: a refusal that
says so, because guessing would be a quiet lie.
