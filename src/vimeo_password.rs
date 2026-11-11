//! Vimeo passwords: the one challenge this build enters.
//!
//! A password-protected video and a password-protected album (a showcase) are
//! two different endpoints on two different hosts, and Vimeo answers both with a
//! *status* rather than a body: `418` from the player for a wrong video
//! password, `401` from the album endpoint. So the two functions here return a
//! session that either carries what the site's own answer carried, or is a
//! refusal that says the password did not work. Nothing retries a wrong
//! password, and nothing guesses one.
//!
//! | What | Where it is proved | What it answers |
//! | --- | --- | --- |
//! | a video at `player.vimeo.com` | `POST <page>/check-password`, `password` = **base64** of the password, form-encoded | the whole player config, as `418` says "wrong" |
//! | an album | `POST https://vimeo.com/showcase/<id>/auth`, `password` and the viewer's `xsrft` token, form-encoded | `hashed_pass`, which the album's video list then takes as `_hashed_pass` |
//!
//! The password itself is never a command-line argument. `tfas --help` is
//! printed into bug reports and pasted into chat windows, and a shell's own
//! `ps` shows every argument to every local user; a password typed there is a
//! password published. [`from_environment`] reads `TAFAS_VIDEO_PASSWORD` (a
//! password) or `TAFAS_VIDEO_PASSWORD_FILE` (a file holding one line), which is
//! what a CI job and a shell script need, and is the reason `docs/roadmap.md`
//! no longer says "no `TAFAS_*` variables".

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::Http;
use crate::sites::text;
use crate::url::percent_encode;

/// The variable that holds the password itself.
pub const PASSWORD_ENV: &str = "TAFAS_VIDEO_PASSWORD";
/// The variable that names a file holding one line: the password and a newline.
/// Preferred over the variable above, because an environment is readable from
/// `/proc/<pid>/environ` by anything running as the same user.
pub const PASSWORD_FILE_ENV: &str = "TAFAS_VIDEO_PASSWORD_FILE";

/// The password this run was given, if it was given one.
///
/// `None` means the user said nothing, and every refusal that mentions a
/// password keeps saying the video is protected without one — the flag is not
/// required for a public video, and a missing password is not an error until a
/// page turns out to need one.
pub fn from_environment() -> Option<String> {
    if let Some(path) = std::env::var_os(PASSWORD_FILE_ENV) {
        // A file that cannot be read is *not* silently "no password": the user
        // named a file, and the refusal says the file is the problem.
        return match std::fs::read_to_string(&path) {
            Ok(text) => {
                let password = text.trim_end_matches(['\r', '\n']).to_string();
                if password.is_empty() {
                    None
                } else {
                    Some(password)
                }
            }
            Err(error) => {
                // Reported through the same channel as a refusal with a name, so
                // the run stops before any request rather than at the password.
                eprintln!(
                    "tafas: {PASSWORD_FILE_ENV}={}: could not be read ({error})",
                    path.to_string_lossy()
                );
                None
            }
        };
    }
    std::env::var(PASSWORD_ENV)
        .ok()
        .map(|password| password.trim_end_matches(['\r', '\n']).to_string())
        .filter(|password| !password.is_empty())
}

/// The one line of a password file: the password, without the newline a shell
/// leaves on it. Split out so a test can check it without a file.
pub fn password_from_file(text: &str) -> Option<String> {
    let password = text.trim_end_matches(['\r', '\n']).to_string();
    if password.is_empty() {
        None
    } else {
        Some(password)
    }
}

/// Standard base64 with padding, which is what the player's endpoint takes.
///
/// Written here rather than pulled in: it is twenty lines against a dependency,
/// the encoder never leaves this file, and `base64` in the tree would be a crate
/// added to satisfy one request body.
pub fn base64(input: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let triple = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

/// The body Vimeo's player expects at `check-password`: the password, base64'd,
/// form-encoded.
pub fn video_password_body(password: &str) -> String {
    format!("password={}", percent_encode(&base64(password)))
}

/// What the player answers for a password.
pub enum VideoAnswer {
    /// The player config, which is what the *unprotected* video gets as well.
    Configuration(Value),
    Wrong,
}

/// Proves a video password against the page the video lives on.
///
/// `page_url` is the address that asked for the password — the player's own
/// `redirect_url` — with its query removed: the endpoint is
/// `<that address>/check-password`, and a query on the end of it is a different
/// resource.
pub fn unlock_video(http: &Http, page_url: &str, password: &str) -> Result<VideoAnswer> {
    let page = page_url.split(['?', '#']).next().unwrap_or(page_url);
    let endpoint = format!("{page}/check-password");
    let body = video_password_body(password);
    let status = http.status_of(
        "vimeo-password",
        &endpoint,
        reqwest::Method::POST,
        &[("referer", page)],
        Some((
            "application/x-www-form-urlencoded",
            body.clone().into_bytes(),
        )),
    )?;
    if status == reqwest::StatusCode::IM_A_TEAPOT {
        // 418 is the site's own "wrong password", not a joke and not a typo.
        return Ok(VideoAnswer::Wrong);
    }
    if !status.is_success() {
        return Err(Error::Refused(format!(
            "vimeo: {page}: the password endpoint answered HTTP {}; the password was not \
             accepted and the video cannot be read",
            status.as_u16()
        )));
    }
    let config = http.json_request(
        "vimeo-password",
        &endpoint,
        reqwest::Method::POST,
        &[("referer", page)],
        Some(("application/x-www-form-urlencoded", body.into_bytes())),
    )?;
    Ok(VideoAnswer::Configuration(config))
}

/// The `hashed_pass` an album hands back for a correct password, or `None` for a
/// wrong one.
pub fn unlock_album(
    http: &Http,
    album_id: &str,
    password: &str,
    xsrft: Option<&str>,
) -> Result<Option<String>> {
    let endpoint = format!("https://vimeo.com/showcase/{album_id}/auth");
    let mut body = format!("password={}", percent_encode(password));
    if let Some(token) = xsrft {
        if !token.is_empty() {
            body.push_str("&token=");
            body.push_str(&percent_encode(token));
        }
    }
    let status = http.status_of(
        "vimeo-album-password",
        &endpoint,
        reqwest::Method::POST,
        &[("x-requested-with", "XMLHttpRequest")],
        Some((
            "application/x-www-form-urlencoded",
            body.clone().into_bytes(),
        )),
    )?;
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(Error::Refused(format!(
            "vimeo: album {album_id}: the password endpoint answered HTTP {}; the password was \
             not accepted and the album cannot be read",
            status.as_u16()
        )));
    }
    let answer = http.json_request(
        "vimeo-album-password",
        &endpoint,
        reqwest::Method::POST,
        &[("x-requested-with", "XMLHttpRequest")],
        Some(("application/x-www-form-urlencoded", body.into_bytes())),
    )?;
    Ok(text(&answer, &["hashed_pass"]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_file_loses_its_trailing_newline_and_nothing_else() {
        assert_eq!(
            password_from_file("correct horse battery staple\n").as_deref(),
            Some("correct horse battery staple")
        );
        assert_eq!(
            password_from_file("with trailing spaces  \r\n").as_deref(),
            Some("with trailing spaces  "),
            "only the line ending is removed; spaces are part of the password"
        );
        assert_eq!(
            password_from_file("\n"),
            None,
            "a blank file is no password"
        );
        assert_eq!(password_from_file(""), None);
    }

    #[test]
    fn base64_is_the_standard_alphabet_with_padding() {
        // The vectors every encoder is checked against, one per padding length.
        assert_eq!(base64(""), "");
        assert_eq!(base64("f"), "Zg==");
        assert_eq!(base64("fo"), "Zm8=");
        assert_eq!(base64("foo"), "Zm9v");
        assert_eq!(base64("foob"), "Zm9vYg==");
        assert_eq!(base64("fooba"), "Zm9vYmE=");
        assert_eq!(base64("foobar"), "Zm9vYmFy");
        // A password is bytes, not text: an accented character is two of them.
        assert_eq!(base64("é"), "w6k=");
    }

    #[test]
    fn the_video_body_is_base64_and_form_encoded() {
        // `base64("hi")` is `aGk=`, and the `=` is the one character in the
        // alphabet that would end the form value early.
        assert_eq!(video_password_body("hi"), "password=aGk%3D");
        assert_eq!(
            video_password_body("a b"),
            format!("password={}", percent_encode(&base64("a b")))
        );
        assert!(
            !video_password_body("a&b").contains("&b"),
            "a password cannot add a second form field"
        );
    }
}
