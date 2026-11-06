use std::io::{Read, Write};
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use reqwest::StatusCode;
use serde_json::Value;

use crate::cookies::Jar;
use crate::error::{Error, Result};
use crate::url::Url;

/// Sent with everything that is not a media request. The three sites serve a
/// different page to a browser user agent, and a different page again to
/// `curl`; the browser string is the one that carries the data we need.
pub const UA_DESKTOP: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                              (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// The InnerTube clients identify themselves with their own user agents; these
/// two are the strings those clients actually send.
pub const UA_ANDROID: &str = "com.google.android.youtube/19.44.38 (Linux; U; Android 14) gzip";
pub const UA_IOS: &str =
    "com.google.ios.youtube/19.45.4 (iPhone16,2; U; CPU iOS 18_1_0 like Mac OS X;)";

/// A JSON response larger than this is not a page we can use; it is a mistake
/// or a machine-generated flood, and reading it into memory is the attack.
const MAX_JSON_BYTES: u64 = 16 * 1024 * 1024;
/// HTML pages: the YouTube watch page is about 1.5 MiB with the player
/// response inlined, and TikTok's is larger. Bounded anyway.
const MAX_HTML_BYTES: u64 = 24 * 1024 * 1024;
/// How much of an error body is quoted back to the user.
const MAX_ERROR_BODY: usize = 512;

/// What a completed (or resumed) stream download reports.
#[derive(Debug, Clone)]
pub struct DownloadReport {
    /// Bytes that landed in the sink during this call.
    pub written: u64,
    /// Total size of the object, when the server said so.
    pub total: Option<u64>,
    /// True when a requested resume was ignored and the body started at zero;
    /// the caller must then treat the sink as a fresh file.
    pub restarted: bool,
    pub final_url: String,
}

/// The one HTTP client in the process.
///
/// It exists to concentrate three decisions in one place: what is retried
/// (transport errors, 429 and 5xx — never 4xx), how much of a response may be
/// read into memory, and which failures are refusals rather than errors.
/// reqwest is built with rustls and the `ring` provider, so no system TLS
/// library is involved and the binary links nothing but libc.
pub struct Http {
    client: Client,
    retries: u32,
    timeout: Duration,
    verbose: u8,
    /// The cookies the user named with `--cookies`, and nothing else. tafas
    /// stores none of its own: a cookie is a credential, and a credential this
    /// program invented would be one it has to keep.
    cookies: Option<Jar>,
}

/// rustls refuses to guess between crypto providers, and reqwest is built with
/// `rustls-tls-webpki-roots-no-provider` on purpose: the choice is made here.
/// Without this call a build where more than one provider is reachable panics
/// with "No provider set" the first time a TLS connection is opened.
fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // An error only means something else in the process got there first,
        // which is the same outcome.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

impl Http {
    pub fn new(retries: u32, timeout_secs: u64, verbose: u8) -> Result<Http> {
        install_crypto_provider();
        let client = Client::builder()
            .user_agent(UA_DESKTOP)
            .connect_timeout(Duration::from_secs(timeout_secs.clamp(1, 30)))
            // Redirects are followed here rather than by hand: `t.co` and
            // `vm.tiktok.com` are pure redirectors and the extractors need the
            // final URL. The limit is reqwest's default ten, made explicit.
            .redirect(reqwest::redirect::Policy::limited(10))
            .gzip(true)
            // No global timeout: a 500 MiB video on a slow line is not an error.
            // Whole-request deadlines are applied to the metadata calls, which
            // are small and must not hang a pipeline.
            .build()
            .map_err(|e| Error::Internal(format!("could not build the HTTP client: {e}")))?;
        Ok(Http {
            client,
            retries,
            timeout: Duration::from_secs(timeout_secs.max(1)),
            verbose,
            cookies: None,
        })
    }

    /// Attaches a jar to every request this client makes.
    pub fn with_cookies(mut self, jar: Jar) -> Http {
        self.cookies = Some(jar);
        self
    }

    /// A GET with the caller's headers and any cookie that applies to `url`.
    ///
    /// The cookie header is computed per request from the URL rather than set
    /// once on the client, which is what keeps a jar scoped to one site from
    /// following a redirect to a CDN on another.
    fn prepare(&self, url: &str, headers: &[(&str, &str)]) -> reqwest::blocking::RequestBuilder {
        self.prepare_with(self.client.get(url), url, headers)
    }

    /// The same, for a request that has already chosen its method.
    fn prepare_with(
        &self,
        mut request: reqwest::blocking::RequestBuilder,
        url: &str,
        headers: &[(&str, &str)],
    ) -> reqwest::blocking::RequestBuilder {
        let mut explicit: Option<String> = None;
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("cookie") {
                explicit = Some((*value).to_string());
                continue;
            }
            request = request.header(*name, *value);
        }
        let from_jar = self
            .cookies
            .as_ref()
            .and_then(|jar| Url::parse(url).ok().and_then(|url| jar.header(&url)));
        let cookie = match (explicit, from_jar) {
            (Some(explicit), Some(jar)) => Some(format!("{explicit}; {jar}")),
            (explicit, jar) => explicit.or(jar),
        };
        if let Some(cookie) = cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        request
    }

    fn log(&self, text: &str) {
        if self.verbose > 0 {
            eprintln!("tafas: {text}");
        }
    }

    /// Runs a request with the retry policy. `build` is called once per
    /// attempt, because a `RequestBuilder` that has already been sent cannot be
    /// reused.
    fn execute<F>(&self, label: &str, build: F) -> Result<Response>
    where
        F: Fn() -> reqwest::blocking::RequestBuilder,
    {
        let mut attempt = 0u32;
        loop {
            let request = build().timeout(self.timeout);
            let result = request.send();
            match result {
                Ok(response) => {
                    let status = response.status();
                    if retryable_status(status) && attempt < self.retries {
                        let wait = backoff(attempt);
                        self.log(&format!(
                            "{label}: HTTP {} — retrying in {:.1}s ({}/{})",
                            status.as_u16(),
                            wait.as_secs_f64(),
                            attempt + 1,
                            self.retries
                        ));
                        std::thread::sleep(wait);
                        attempt += 1;
                        continue;
                    }
                    return Ok(response);
                }
                Err(e) => {
                    if attempt < self.retries && retryable_error(&e) {
                        let wait = backoff(attempt);
                        self.log(&format!(
                            "{label}: {e} — retrying in {:.1}s ({}/{})",
                            wait.as_secs_f64(),
                            attempt + 1,
                            self.retries
                        ));
                        std::thread::sleep(wait);
                        attempt += 1;
                        continue;
                    }
                    return Err(Error::from(e));
                }
            }
        }
    }

    /// Turns a non-success status into the error class it belongs to, quoting a
    /// bounded slice of the body so that a 403 that says "bot" is not reported
    /// as a mystery.
    fn classify(&self, label: &str, url: &str, response: Response) -> Result<Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = read_bounded(response, MAX_ERROR_BODY as u64).unwrap_or_default();
        let excerpt = first_line(&body);
        let detail = if excerpt.is_empty() {
            String::new()
        } else {
            format!(": {excerpt}")
        };
        let text = format!("{label}: HTTP {} from {url}{detail}", status.as_u16());
        Err(match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Error::Refused(text),
            StatusCode::NOT_FOUND | StatusCode::GONE => Error::Unsupported(text),
            StatusCode::TOO_MANY_REQUESTS => Error::Http(text),
            _ => Error::Http(text),
        })
    }

    /// A page as text, with a hard ceiling.
    pub fn get_text(&self, url: &str, headers: &[(&str, &str)]) -> Result<String> {
        let response = self.execute("page", || self.prepare(url, headers))?;
        let response = self.classify("page", url, response)?;
        read_bounded(response, MAX_HTML_BYTES)
    }

    /// A small body — one HLS segment, one key — read into memory on purpose:
    /// a segment is seconds of video, and holding one at a time keeps the
    /// memory ceiling flat however many segments the playlist names.
    ///
    /// `range` asks for a window of the object (`EXT-X-BYTERANGE`). A server
    /// that answers `200` with the whole file instead is a refusal, not a
    /// success: the bytes would then start at zero, and the caller would splice
    /// the wrong window into the file.
    pub fn get_bytes(
        &self,
        label: &str,
        url: &str,
        headers: &[(&str, &str)],
        range: Option<(u64, u64)>,
        limit: u64,
    ) -> Result<Vec<u8>> {
        let response = self.execute(label, || {
            let mut request = self.prepare(url, headers);
            if let Some((offset, length)) = range {
                let end = offset.saturating_add(length.saturating_sub(1));
                request = request.header(reqwest::header::RANGE, format!("bytes={offset}-{end}"));
            }
            request
        })?;
        let response = self.classify(label, url, response)?;

        if let Some((offset, _)) = range {
            if offset > 0 && response.status() != StatusCode::PARTIAL_CONTENT {
                return Err(Error::Http(format!(
                    "{label}: the server ignored a byte-range request for {url}"
                )));
            }
        }

        // A server may answer a range with more than was asked for — everything
        // from `offset` to the end of the object is legal — so the window is cut
        // here rather than trusted to the server.
        let ceiling = match range {
            Some((_, length)) => length.min(limit),
            None => limit,
        };
        let mut buffer = Vec::new();
        response
            .take(ceiling.saturating_add(1))
            .read_to_end(&mut buffer)
            .map_err(|e| Error::Http(format!("{label}: could not read {url}: {e}")))?;

        if let Some((offset, length)) = range {
            if (buffer.len() as u64) < length {
                return Err(Error::Http(format!(
                    "{label}: asked for {length} bytes at {offset} of {url}, received {}",
                    buffer.len()
                )));
            }
            buffer.truncate(length as usize);
            return Ok(buffer);
        }
        if buffer.len() as u64 > limit {
            return Err(Error::Limit(format!(
                "{label}: {url} is larger than {} and is not a segment",
                crate::progress::human_bytes(limit)
            )));
        }
        Ok(buffer)
    }

    /// A JSON document from a GET. `Ok(None)` means the endpoint answered 404,
    /// which two of the three sites use to mean "this post is gone" rather than
    /// "your request is broken".
    pub fn get_json_opt(
        &self,
        label: &str,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<Option<Value>> {
        let response = self.execute(label, || {
            self.prepare(url, headers)
                .header("accept", "application/json")
        })?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = self.classify(label, url, response)?;
        let text = read_bounded(response, MAX_JSON_BYTES)?;
        Ok(Some(parse_json(label, &text)?))
    }

    pub fn get_json(&self, label: &str, url: &str, headers: &[(&str, &str)]) -> Result<Value> {
        self.get_json_opt(label, url, headers)?
            .ok_or_else(|| Error::Unsupported(format!("{label}: HTTP 404 from {url}")))
    }

    /// A JSON document from a POST, which is how InnerTube is spoken to.
    pub fn post_json(
        &self,
        label: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> Result<Value> {
        let payload = serde_json::to_string(body)
            .map_err(|e| Error::Internal(format!("could not encode the {label} request: {e}")))?;
        let response = self.execute(label, || {
            let post = self
                .client
                .post(url)
                .header("content-type", "application/json");
            self.prepare_with(post, url, headers).body(payload.clone())
        })?;
        let response = self.classify(label, url, response)?;
        let text = read_bounded(response, MAX_JSON_BYTES)?;
        parse_json(label, &text)
    }

    /// Sends a request and reports where it ended up, without keeping the body.
    /// Used only for the short links (`t.co`, `vm.tiktok.com`), where the whole
    /// purpose of the link is its destination.
    pub fn resolve(&self, url: &str) -> Result<String> {
        let response = self.execute("redirect", || self.prepare(url, &[]))?;
        let final_url = response.url().to_string();
        // Drop the body without reading it: the connection is reused only for
        // its headers, and a short link's destination is enough.
        drop(response);
        if final_url.is_empty() {
            return Err(Error::Http(format!("could not resolve {url}")));
        }
        self.log(&format!("{url} → {final_url}"));
        Ok(final_url)
    }

    /// Streams a URL into `sink`.
    ///
    /// `resume_from` is the number of bytes already in the sink: a Range
    /// request is issued, and a server that ignores it (`200` instead of `206`)
    /// is reported as `restarted` so the caller truncates rather than appending
    /// a second copy of the beginning.
    #[allow(clippy::too_many_arguments)]
    pub fn download(
        &self,
        label: &str,
        url: &str,
        headers: &[(&str, &str)],
        sink: &mut dyn Write,
        resume_from: u64,
        on_bytes: &mut dyn FnMut(u64),
    ) -> Result<DownloadReport> {
        let response = self.execute(label, || {
            let mut request = self.prepare(url, headers);
            if resume_from > 0 {
                request = request.header("range", format!("bytes={resume_from}-"));
            }
            request
        })?;

        let status = response.status();
        let final_url = response.url().to_string();

        // 416 means the range starts past the end. With a resume in flight that
        // is either a finished file or a stale `.part`; both are handled by
        // asking for the whole thing again from zero.
        if status == StatusCode::RANGE_NOT_SATISFIABLE && resume_from > 0 {
            return Ok(DownloadReport {
                written: 0,
                total: None,
                restarted: true,
                final_url,
            });
        }
        let response = self.classify(label, url, response)?;

        let restarted = resume_from > 0 && status != StatusCode::PARTIAL_CONTENT;
        let total = if restarted {
            content_length(&response)
        } else {
            content_length(&response).map(|len| len + resume_from)
        };

        let mut reader = response;
        let mut buffer = vec![0u8; 64 * 1024];
        let mut written = 0u64;
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            sink.write_all(&buffer[..read])?;
            written += read as u64;
            on_bytes(read as u64);
        }

        Ok(DownloadReport {
            written,
            total,
            restarted,
            final_url,
        })
    }

    pub fn verbose(&self) -> u8 {
        self.verbose
    }
}

fn parse_json(label: &str, text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|e| {
        // A truncated or re-shaped document is the normal way a site changes;
        // the first character of the body in the message is what tells an
        // operator that they were served HTML instead of JSON.
        let head: String = text.chars().take(120).collect();
        Error::Parse(format!(
            "{label}: response is not JSON ({e}); body starts with {head:?}"
        ))
    })
}

fn content_length(response: &Response) -> Option<u64> {
    response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

fn read_bounded(mut response: Response, limit: u64) -> Result<String> {
    let mut buffer = Vec::new();
    let read = response
        .by_ref()
        .take(limit)
        .read_to_end(&mut buffer)
        .map_err(|e| Error::Http(format!("could not read the response body: {e}")))?;
    let _ = read;
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

fn retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn retryable_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || (error.is_request() && !error.is_body())
}

fn backoff(attempt: u32) -> Duration {
    // 0.4 s, 0.8 s, 1.6 s … capped. Deterministic on purpose: a fixed schedule
    // is testable, and the sites do not benefit from jitter here.
    let millis = 400u64 << attempt.min(4);
    Duration::from_millis(millis.min(5_000))
}

fn first_line(body: &str) -> String {
    body.lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_stops() {
        assert_eq!(backoff(0), Duration::from_millis(400));
        assert_eq!(backoff(1), Duration::from_millis(800));
        assert_eq!(backoff(9), Duration::from_millis(5_000));
    }

    #[test]
    fn only_transport_and_server_errors_are_retried() {
        assert!(retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(retryable_status(StatusCode::BAD_GATEWAY));
        assert!(!retryable_status(StatusCode::FORBIDDEN));
        assert!(!retryable_status(StatusCode::NOT_FOUND));
    }
}
