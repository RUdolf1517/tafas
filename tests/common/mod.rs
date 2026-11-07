//! A tiny HTTP/1.1 server, on purpose.
//!
//! The transfer layer's interesting behaviour is what happens when a server
//! misbehaves: a truncated body, a `Range` request that is ignored, a `500` that
//! clears on the second attempt. Mocking those is easy and proves nothing, so
//! the tests speak to a real socket instead. It is built from `std::net` and
//! binds to port 0, so a parallel test run cannot collide with it and nothing
//! outside the machine is involved.

#![allow(dead_code)]

pub mod mp4;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// One path, and how it should behave.
pub struct Route {
    pub path: String,
    pub body: Vec<u8>,
    /// Answer this many requests with a 5xx before answering normally.
    pub fail_first: usize,
    /// Honour `Range` with a 206, or ignore it with a 200.
    pub support_range: bool,
    /// Truncate the declared body, to exercise the byte-count check.
    pub truncate_by: usize,
    /// Answer only when the request carries this header with this value, and
    /// `403` otherwise: the shape a site with a session check has.
    pub requires_header: Option<(String, String)>,
    /// Hold the response for this long. A test that fetches segments in
    /// parallel needs the requests to overlap on the server, and a delay is a
    /// larger signal than a wall-clock assertion on a loaded runner.
    pub delay_ms: u64,
}

impl Route {
    pub fn ok(path: &str, body: &[u8]) -> Route {
        Route {
            path: path.to_string(),
            body: body.to_vec(),
            fail_first: 0,
            support_range: true,
            truncate_by: 0,
            requires_header: None,
            delay_ms: 0,
        }
    }

    /// A route that answers after `delay_ms`, so two requests can be in flight
    /// at once even when the body is tiny.
    pub fn slow(path: &str, body: &[u8], delay_ms: u64) -> Route {
        Route {
            delay_ms,
            ..Route::ok(path, body)
        }
    }

    /// A route that answers `403` unless the request carries `name: value`.
    pub fn requiring_header(path: &str, name: &str, value: &str, body: &[u8]) -> Route {
        Route {
            requires_header: Some((name.to_lowercase(), value.to_string())),
            ..Route::ok(path, body)
        }
    }

    pub fn failing(path: &str, body: &[u8], times: usize) -> Route {
        Route {
            fail_first: times,
            ..Route::ok(path, body)
        }
    }

    pub fn ignoring_range(path: &str, body: &[u8]) -> Route {
        Route {
            support_range: false,
            ..Route::ok(path, body)
        }
    }

    pub fn truncated(path: &str, body: &[u8], by: usize) -> Route {
        Route {
            truncate_by: by,
            ..Route::ok(path, body)
        }
    }
}

pub struct TestServer {
    pub port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
    pub requests: Arc<AtomicUsize>,
    /// Requests per path, so a test can assert that a resource was fetched
    /// once rather than counting connections.
    hits: Arc<Mutex<HashMap<String, usize>>>,
    /// Requests being served right now, and the highest number seen at once.
    live: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl TestServer {
    pub fn start(routes: Vec<Route>) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a test port");
        let port = listener.local_addr().expect("local address").port();
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");

        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let hits = Arc::new(Mutex::new(HashMap::new()));
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let stop_flag = Arc::clone(&stop);
        let counter = Arc::clone(&requests);
        let path_hits = Arc::clone(&hits);
        let live_now = Arc::clone(&live);
        let peak_now = Arc::clone(&peak);
        let routes = Arc::new(routes);

        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        counter.fetch_add(1, Ordering::SeqCst);
                        // One thread per connection, so a client that asks for
                        // several things at once is served them at once.
                        let routes = Arc::clone(&routes);
                        let path_hits = Arc::clone(&path_hits);
                        let live = Arc::clone(&live_now);
                        let peak = Arc::clone(&peak_now);
                        thread::spawn(move || {
                            let _ = handle_connection(
                                stream,
                                routes.as_slice(),
                                &path_hits,
                                &live,
                                &peak,
                            );
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        TestServer {
            port,
            stop,
            handle: Some(handle),
            requests,
            hits,
            live,
            peak,
        }
    }

    /// The most requests that were ever held back at the same moment. Only
    /// routes built with `Route::slow` are counted, because those are the ones
    /// whose overlap is a fact rather than a race.
    pub fn peak_concurrent(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    /// How many requests a path received.
    pub fn hits(&self, path: &str) -> usize {
        self.hits
            .lock()
            .map(|hits| hits.get(path).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // A connect wakes the accept loop so the thread can see the flag.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    routes: &[Route],
    hits: &Mutex<HashMap<String, usize>>,
    live: &AtomicUsize,
    peak: &AtomicUsize,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();

    let mut range_start: Option<u64> = None;
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_lowercase(), value.trim().to_string()));
        }
        if let Some(value) = line
            .strip_prefix("Range: ")
            .or_else(|| line.strip_prefix("range: "))
        {
            if let Some(spec) = value.strip_prefix("bytes=") {
                if let Some(start) = spec.split('-').next() {
                    range_start = start.trim().parse::<u64>().ok();
                }
            }
        }
    }

    if method != "GET" {
        return write_response(&mut stream, 405, "Method Not Allowed", b"", None);
    }

    let path = target.split('?').next().unwrap_or("/");
    if let Ok(mut hits) = hits.lock() {
        *hits.entry(path.to_string()).or_insert(0) += 1;
    }
    let route = match routes.iter().find(|route| route.path == path) {
        Some(route) => route,
        None => return write_response(&mut stream, 404, "Not Found", b"no route", None),
    };

    // Failures are counted per process, not per route, which is enough for a
    // retry test: the second request finds the counter exhausted.
    static FAILURES: AtomicUsize = AtomicUsize::new(0);
    // `truncate_by` keeps the declared length honest and cuts the bytes sent,
    // which is what a connection dropped mid-transfer actually looks like.
    let body = route.body.clone();
    let sent = body.len().saturating_sub(route.truncate_by);

    if let Some((name, value)) = &route.requires_header {
        let carried = headers
            .iter()
            .filter(|(header, _)| header == name)
            .any(|(_, carried)| carried.split(';').any(|part| part.trim() == value));
        if !carried {
            return write_response(&mut stream, 403, "Forbidden", b"no session", None);
        }
    }

    // The delay is where a request is observably in flight. Counting it here —
    // rather than around the whole connection — is what makes "one at a time"
    // and "two at a time" deterministic instead of a race with the client
    // reading the answer and asking for the next segment.
    if route.delay_ms > 0 {
        let in_flight = live.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(in_flight, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(route.delay_ms));
        live.fetch_sub(1, Ordering::SeqCst);
    }

    let already = FAILURES.load(Ordering::SeqCst);
    if already < route.fail_first {
        FAILURES.store(already + 1, Ordering::SeqCst);
        return write_response(
            &mut stream,
            500,
            "Internal Server Error",
            b"try again",
            None,
        );
    }

    match range_start {
        Some(start) if route.support_range && start < body.len() as u64 => {
            let slice = &body[start as usize..];
            write_partial(&mut stream, slice, start, body.len() as u64)
        }
        Some(_) if route.support_range => {
            // A range past the end is what a finished file looks like.
            write_response(&mut stream, 416, "Range Not Satisfiable", b"", None)
        }
        _ if route.truncate_by > 0 => write_short(&mut stream, body.len(), sent, &body),
        _ => write_response(&mut stream, 200, "OK", &body, None),
    }
}

/// Declares `declared` bytes and sends only `sent`, then closes: a transfer
/// that ends early, which is what the caller's length check exists for.
fn write_short(
    stream: &mut TcpStream,
    declared: usize,
    sent: usize,
    body: &[u8],
) -> std::io::Result<()> {
    let head =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&body[..sent])?;
    stream.flush()
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    body: &[u8],
    extra: Option<&str>,
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(extra) = extra {
        head.push_str(extra);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn write_partial(
    stream: &mut TcpStream,
    body: &[u8],
    start: u64,
    total: u64,
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
        body.len(),
        start,
        total - 1,
        total
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// Reads a file the test wrote, for the cases where the assertion is about
/// bytes rather than about the exit code.
pub fn read(path: &std::path::Path) -> Vec<u8> {
    let mut file = std::fs::File::open(path).expect("open the written file");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).expect("read the written file");
    bytes
}

/// A fresh directory per test, removed by the caller when it is done.
pub fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tafas-test-{}-{}-{}",
        std::process::id(),
        name,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("create the temp dir");
    dir
}

/// A whole transport stream in a handful of packets: a program table, a program
/// map, two H.264 access units and two AAC frames.
///
/// It is the smallest stream the demuxer accepts with both tracks in it, which
/// is what an end-to-end HLS test needs — the playlist and segment plumbing is
/// under test, not the codec.
pub fn ts_segment() -> Vec<u8> {
    const SYNC: u8 = 0x47;
    const PACKET: usize = 188;

    fn packet(pid: u16, unit_start: bool, counter: u8, payload: &[u8]) -> Vec<u8> {
        assert!(
            payload.len() <= PACKET - 4,
            "a packet holds 184 payload bytes"
        );
        let mut out = Vec::with_capacity(PACKET);
        out.push(SYNC);
        out.push((if unit_start { 0x40 } else { 0x00 }) | ((pid >> 8) as u8 & 0x1f));
        out.push(pid as u8);
        let stuffing = PACKET - 4 - payload.len();
        if stuffing == 0 {
            out.push(0x10 | (counter & 0x0f));
        } else {
            out.push(0x30 | (counter & 0x0f));
            let field_length = stuffing - 1;
            out.push(field_length as u8);
            if field_length > 0 {
                out.push(0x00);
                out.resize(out.len() + field_length - 1, 0xff);
            }
        }
        out.extend_from_slice(payload);
        assert_eq!(out.len(), PACKET);
        out
    }

    fn pat(program_map_pid: u16) -> Vec<u8> {
        let mut section = vec![0u8; 8];
        section[0] = 0x00;
        section[1] = 0xb0;
        section[2] = 0x0d;
        let mut entry = [0u8; 4];
        entry[0..2].copy_from_slice(&1u16.to_be_bytes());
        entry[2] = 0xe0 | ((program_map_pid >> 8) as u8 & 0x1f);
        entry[3] = program_map_pid as u8;
        section.extend_from_slice(&entry);
        let mut payload = vec![0u8];
        payload.extend_from_slice(&section);
        payload
    }

    fn pmt(video_pid: u16, audio_pid: u16) -> Vec<u8> {
        let mut section = vec![0u8; 12];
        section[0] = 0x02;
        section[1] = 0xb0;
        section[2] = 0x17;
        section.push(0x1b);
        section.push(0xe0 | ((video_pid >> 8) as u8 & 0x1f));
        section.push(video_pid as u8);
        section.push(0xf0);
        section.push(0x00);
        section.push(0x0f);
        section.push(0xe0 | ((audio_pid >> 8) as u8 & 0x1f));
        section.push(audio_pid as u8);
        section.push(0xf0);
        section.push(0x00);
        let mut payload = vec![0u8];
        payload.extend_from_slice(&section);
        payload
    }

    fn timestamp_bytes(prefix: u8, value: u64) -> [u8; 5] {
        [
            prefix | (((value >> 30) as u8 & 0x07) << 1) | 0x01,
            ((value >> 22) & 0xff) as u8,
            (((value >> 15) & 0x7f) as u8) << 1 | 0x01,
            ((value >> 7) & 0xff) as u8,
            ((value & 0x7f) as u8) << 1 | 0x01,
        ]
    }

    fn pes(stream_id: u8, pts: u64, dts: Option<u64>, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x00, 0x00, 0x01, stream_id];
        out.extend_from_slice(&0u16.to_be_bytes());
        out.push(0x80);
        out.push(if dts.is_some() { 0xc0 } else { 0x80 });
        out.push(if dts.is_some() { 10 } else { 5 });
        out.extend_from_slice(&timestamp_bytes(
            if dts.is_some() { 0x30 } else { 0x20 },
            pts,
        ));
        if let Some(dts) = dts {
            out.extend_from_slice(&timestamp_bytes(0x10, dts));
        }
        out.extend_from_slice(payload);
        out
    }

    const SPS: [u8; 7] = [0x67, 0x42, 0x00, 0x1e, 0x95, 0xa8, 0x14];
    const PPS: [u8; 4] = [0x68, 0xce, 0x3c, 0x80];
    const IDR: [u8; 5] = [0x65, 0x88, 0x84, 0x00, 0x0a];

    fn video_unit() -> Vec<u8> {
        let mut out = Vec::new();
        for unit in [&SPS[..], &PPS[..], &IDR[..]] {
            out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            out.extend_from_slice(unit);
        }
        out
    }

    fn adts_frame(payload: &[u8]) -> Vec<u8> {
        let length = payload.len() + 7;
        let mut out = vec![0u8; 7];
        out[0] = 0xff;
        out[1] = 0xf1;
        out[2] = (1 << 6) | (4 << 2); // AAC-LC, 44100 Hz
        out[3] = ((2 & 0x03) << 6) | ((length >> 11) as u8 & 0x03);
        out[4] = (length >> 3) as u8;
        out[5] = (((length & 0x07) as u8) << 5) | 0x1f;
        out[6] = 0xfc;
        out.extend_from_slice(payload);
        out
    }

    let mut out = Vec::new();
    out.extend_from_slice(&packet(0, true, 0, &pat(4096)));
    out.extend_from_slice(&packet(4096, true, 0, &pmt(481, 482)));
    out.extend_from_slice(&packet(
        481,
        true,
        0,
        &pes(0xe0, 90_000, Some(90_000), &video_unit()),
    ));
    out.extend_from_slice(&packet(
        481,
        true,
        1,
        &pes(0xe0, 93_000, Some(93_000), &video_unit()),
    ));
    let audio = adts_frame(&[0x21, 0x10, 0x05]);
    out.extend_from_slice(&packet(482, true, 0, &pes(0xc0, 90_000, None, &audio)));
    out.extend_from_slice(&packet(482, true, 1, &pes(0xc0, 93_000, None, &audio)));
    out
}
