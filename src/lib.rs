#![forbid(unsafe_code)]

//! `tafas` — capture a video from YouTube, TikTok or X as one playable file.
//!
//! The crate is a library with a thin binary on top, because the parts that
//! deserve tests — URL parsing, stream selection, naming, the MP4 remuxer — are
//! not the parts that need a terminal. `main.rs` never contains logic that the
//! library does not expose.

pub mod cli;
pub mod download;
pub mod error;
pub mod hls;
pub mod http;
pub mod model;
pub mod mp4write;
pub mod mux;
pub mod names;
pub mod progress;
pub mod report;
pub mod select;
pub mod sites;
pub mod style;
pub mod ts;
pub mod url;
