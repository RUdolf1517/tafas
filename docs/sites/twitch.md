# Twitch

Hosts: twitch.tv. Detection and the host rules are in
[extractors §1](../extractors.md#1-detection).

Related: [extractors](../extractors.md) · [cli](../cli.md) · [guarantees](../guarantees.md)

This page is the part of [extractors](../extractors.md) that is about Twitch:
its routes, what is selected, and every refusal, with the sentence it produces.
The overview keeps what applies to every site — detection, what a refusal looks
like when it happens, what happens when a site changes, and the
`--extractor-args` escape hatch — so this page can be read on its own.

## 1. Route

One batched GraphQL request carries two operations:

1. `PlaybackAccessToken`, asked by persisted-query hash with the **public web
   client id** in a `client-id` header. It answers `value` and `signature`;
2. `video(id:)` for the title, the uploader, the length and the poster — so a
   capture does not pay a second round trip just to name the file.

Those two values become the playlist URL:
`https://usher.ttvnw.net/vod/<id>.m3u8?...&nauth=<token>&nauthsig=<signature>`.
`value` is a JSON document, so it is percent-encoded before it goes in the query
string; `signature` is a signature over it and is sent as it arrived. `hlsfetch`
then assembles the playlist like any other — the renditions are MPEG-TS, and the
audio-only variant is offered by the same master list.

## 2. Refused

| Situation | Result |
| --- | --- |
| a channel URL (`twitch.tv/<name>`) | unsupported (1): a live stream has no end, and a file needs one |
| `streamPlaybackAccessToken` null | refusal (1): expired, deleted, or subscriber-only; `--cookies FILE` is named |
| a token without `value` or without `signature` | refusal (1) naming which field is missing |
| an `errors` array in the answer | refusal (1) quoting the API's own message, because a changed persisted-query hash arrives exactly that way |

**What it cannot reach:** live channels, clips (`clips.twitch.tv` has no reader
here), subscriber-only VODs without a session, and anything Twitch has already
expired. The client id is public and is not a credential — `SECURITY.md` says so
next to YouTube's key.
