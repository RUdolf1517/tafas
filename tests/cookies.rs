//! `--cookies FILE`, end to end: a jar on disk, a request that needs it.
//!
//! The parsing rules are unit-tested in `src/cookies.rs`. What is tested here is
//! the seam: that the header the client actually sends is the one the jar
//! describes, that nothing is sent where it does not apply, and that a caller's
//! own `cookie` header (YouTube's consent cookies) is not replaced by the jar.

mod common;

use common::{temp_dir, Route, TestServer};
use tafas::cookies::Jar;
use tafas::http::Http;

fn jar(name: &str, text: &str) -> Jar {
    let directory = temp_dir(name);
    let path = directory.join("cookies.txt");
    std::fs::write(&path, text).expect("write the cookie file");
    Jar::load(&path).expect("read the cookie file")
}

fn client(jar: Option<Jar>) -> Http {
    let http = Http::new(1, 5, 0).expect("client");
    match jar {
        Some(jar) => http.with_cookies(jar),
        None => http,
    }
}

#[test]
fn a_cookie_from_the_file_unlocks_the_page() {
    let server = TestServer::start(vec![Route::requiring_header(
        "/page",
        "cookie",
        "SID=abc123",
        b"a page",
    )]);
    let http = client(Some(jar(
        "cookie-hit",
        "127.0.0.1\tTRUE\t/\tFALSE\t0\tSID\tabc123\n",
    )));

    let text = http
        .get_text(&server.url("/page"), &[])
        .expect("the jar should answer the session check");
    assert_eq!(text, "a page");
}

#[test]
fn without_a_jar_the_same_page_is_a_refusal() {
    let server = TestServer::start(vec![Route::requiring_header(
        "/page",
        "cookie",
        "SID=abc123",
        b"a page",
    )]);

    let error = client(None)
        .get_text(&server.url("/page"), &[])
        .expect_err("no cookie, no page");
    assert!(
        error.to_string().contains("403"),
        "a 403 is what a missing session looks like: {error}"
    );
}

#[test]
fn a_jar_for_another_host_sends_nothing() {
    let server = TestServer::start(vec![Route::requiring_header(
        "/page",
        "cookie",
        "SID=abc123",
        b"a page",
    )]);
    let http = client(Some(jar(
        "cookie-elsewhere",
        "example.test\tTRUE\t/\tFALSE\t0\tSID\tabc123\n",
    )));

    assert!(
        http.get_text(&server.url("/page"), &[]).is_err(),
        "the cookie belongs to another host"
    );
}

#[test]
fn a_secure_cookie_stays_off_plain_http() {
    let server = TestServer::start(vec![Route::requiring_header(
        "/page",
        "cookie",
        "SID=abc123",
        b"a page",
    )]);
    let http = client(Some(jar(
        "cookie-secure",
        "127.0.0.1\tTRUE\t/\tTRUE\t0\tSID\tabc123\n",
    )));

    assert!(
        http.get_text(&server.url("/page"), &[]).is_err(),
        "Secure means https only, and the fixture server speaks http"
    );
}

#[test]
fn the_callers_own_cookie_header_keeps_the_jars_cookies() {
    let server = TestServer::start(vec![
        Route::requiring_header("/consent", "cookie", "CONSENT=YES+cb", b"consent"),
        Route::requiring_header("/session", "cookie", "SID=abc123", b"session"),
    ]);
    let http = client(Some(jar(
        "cookie-merge",
        "127.0.0.1\tTRUE\t/\tFALSE\t0\tSID\tabc123\n",
    )));
    let headers = [("cookie", "CONSENT=YES+cb")];

    assert_eq!(
        http.get_text(&server.url("/consent"), &headers)
            .expect("the caller's cookie survives"),
        "consent"
    );
    assert_eq!(
        http.get_text(&server.url("/session"), &headers)
            .expect("the jar's cookie is added beside it"),
        "session"
    );
}
