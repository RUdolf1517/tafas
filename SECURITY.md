# Security Policy

This document explains how to report a security vulnerability in tafas. It does
not replace the safety and guarantee documents linked below, and it is not a
claim that the software is free of vulnerabilities.

## Supported versions

Security reports are investigated against the **latest published release** and
the current **`main` branch**. Fixes are developed on `main` and distributed
through a new release. Backports to older releases are not guaranteed.

Include the release version or source commit in your report so that the affected
code can be identified unambiguously. `tafas --capabilities` prints the version
and a description of the build; it is usually the most useful single line to
attach.

## Report a vulnerability

**Do not disclose an unpatched vulnerability in a public issue, pull request, or
discussion.**

Use either of these private channels:

1. **Preferred:** open the repository's
   [Security tab](https://github.com/RUdolf1517/tafas/security) and choose
   **Report a vulnerability** to submit a private vulnerability report through
   GitHub.
2. **Fallback:** email
   [rudolfzinovev@protonmail.com](mailto:rudolfzinovev@protonmail.com).

You do not need to send the same report through both channels. Do not include
sensitive personal data, cookies, session tokens or private media unless they are
essential to the investigation; a minimal, non-sensitive reproducer is preferred.

## Scope

Reports about any default or opt-in feature are welcome. Security-relevant
examples include:

- an output file committed to the destination that does not match the bytes the
  server sent, or a rename that replaces a file without `--overwrite`;
- writes outside the directory the user selected — path traversal through a
  page title, a `--name` value, or a redirect target;
- bypass of the byte-count or structural verification before commit;
- a crafted page, JSON document or MP4 that causes memory exhaustion, a panic in
  a worker thread that is reported as success, or an out-of-bounds access;
- TLS misbehaviour: a certificate that is accepted when it should not be, or a
  redirect to a different host that a scheme or host check should have stopped;
- a `Range` response accepted in a way that produces a file with the beginning
  written twice;
- bypass of the release-asset checksums in `install.sh`.

A page that changed and now yields a wrong *refusal*, a download that fails
because a site added a check, or a container this build will not remux are
**compatibility** issues. They are real bugs, but they belong in the public
issue tracker.

A documented limitation is not by itself a vulnerability — §7 of
[docs/extractors.md](docs/extractors.md) lists what is deliberately refused. A
way *around* a documented safeguard may be one.

## Credentials in the source, and what to do when a scanner flags one

tafas stores nothing between runs. The only credential it ever handles is the
cookie file named by `--cookies`: it is read once, sent to hosts the cookies'
domains match, never written back and never logged.

One string in the source is shaped like a Google API key and is **not** a
credential: `PUBLIC_WEB_API_KEY` in `src/sites/youtube.rs`. It is the key
YouTube serves to an anonymous browser on every `watch` page and that every
other reader of the InnerTube API ships as a default. It identifies the *client*
to a public endpoint — it unlocks no account, no private video and no quota of
ours, and the only thing associated with it is the caller's IP address. The
watch page's own `INNERTUBE_API_KEY` is preferred whenever the page can be read;
the constant only covers the case where the page could not be fetched at all.

So: when GitHub's secret scanning reports a **Google API Key** in
`src/sites/youtube.rs`, that alert is a false positive. Dismiss it with reason
**"Won't fix"** or **"False positive"** and the comment that the string is
YouTube's public web-client key, not a secret; there is nothing to rotate.
Dismissing it is not a blanket permission to stop looking: any *other* alert
from this repository should be treated as real, and a credential that a user
pastes into an issue or a pull request should be rotated rather than dismissed.
The scan stays enabled for the whole repository — no path is excluded from it —
because excluding a file hides the next real leak as well as this one.

## What to include

Provide as much of the following as is available:

- the version or commit, operating system, architecture, and installation source;
- the exact command, including flags;
- what you expected, what happened, and the security impact;
- verbatim output, with sensitive values removed;
- a minimal reproducer, and — where useful — the response body that triggered it
  (a saved page is better than a URL that may have changed by the time we look).

Do not run a suspected destructive reproducer against valuable data. Use
disposable copies in a test directory, and remember that tafas only writes inside
the directory you name.

## Research and coordinated disclosure

Good-faith research that respects these boundaries is welcome. Test only systems
you own or are authorised to test, do not access or modify third-party data, do
not disrupt services, and do not use more exploitation than is needed to
demonstrate the issue. No rate-limit evasion, no scraping at a scale that burdens
a site, and no third-party account is needed for any of the areas above: every
one of them can be demonstrated against a server you control.

Reports are reviewed and coordinated with the reporter through validation,
remediation, and disclosure. Response and resolution times depend on the problem;
this policy does not promise a fixed response or fix SLA. We aim to keep the
reporter informed and to agree on disclosure timing. Please keep vulnerability
details and proof-of-concept material private until that coordinated disclosure.

## Related documentation

- [Safe file handling](docs/safety.md) — the commit protocol, resume, paths,
  network decisions.
- [Guarantees](docs/guarantees.md) — what is verified, and where it is weaker
  than it sounds.
- [Extractors](docs/extractors.md) — the documented refusals.
- [Dependencies](docs/dependencies.md) — the full dependency set, and where the C
  code is.
- [Releases](docs/releases.md) — artifacts and checksum verification.
