//! A tiny HTTP/1.1 server, on purpose.
//!
//! The transfer layer's interesting behaviour is what happens when a server
//! misbehaves: a truncated body, a `Range` request that is ignored, a `500` that
//! clears on the second attempt. Mocking those is easy and proves nothing, so
//! the tests speak to a real socket instead. It is built from `std::net` and
//! binds to port 0, so a parallel test run cannot collide with it and nothing
//! outside the machine is involved.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
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
}

impl Route {
    pub fn ok(path: &str, body: &[u8]) -> Route {
        Route {
            path: path.to_string(),
            body: body.to_vec(),
            fail_first: 0,
            support_range: true,
            truncate_by: 0,
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
        let stop_flag = Arc::clone(&stop);
        let counter = Arc::clone(&requests);
        let routes = Arc::new(routes);

        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let _ = handle_connection(stream, &routes);
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
        }
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

fn handle_connection(mut stream: TcpStream, routes: &[Route]) -> std::io::Result<()> {
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
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
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
