//! The one line drawn on stderr while a stream is arriving.
//!
//! It is written to stderr, never to stdout, so `tafas URL | …` stays a data
//! pipe. It is drawn only when stderr is a terminal and nothing has asked for
//! silence (`--quiet`, `--json`, `--no-progress`), so a cron job's mail and a
//! CI log do not collect carriage returns.
//!
//! The drawing itself is deliberately fixed-width and redrawn in place: a
//! progress bar that scrolls is worse than none, because it turns a log into
//! thousands of lines that all say the same thing.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

const BAR_WIDTH: usize = 24;

/// A bar narrower than this says nothing a percentage does not already say.
const MIN_BAR_WIDTH: usize = 4;

/// Below this the terminal is not a terminal; `COLUMNS` that small is noise.
const MIN_TERMINAL: usize = 24;
/// Redraw rate. Ten frames a second is smooth to the eye and costs one `write`
/// per frame, which is nothing next to a 64 KiB read.
const FRAME: Duration = Duration::from_millis(100);

pub struct Progress {
    label: String,
    total: Option<u64>,
    done: u64,
    started: Instant,
    last_draw: Option<Instant>,
    drawn: bool,
    enabled: bool,
}

impl Progress {
    pub fn new(label: impl Into<String>, total: Option<u64>, enabled: bool) -> Progress {
        Progress {
            label: label.into(),
            total,
            done: 0,
            started: Instant::now(),
            last_draw: None,
            drawn: false,
            enabled: enabled && std::io::stderr().is_terminal(),
        }
    }

    /// A progress object that never draws. Used for `--quiet`, `--json` and
    /// `--no-progress`, and by every unit test.
    pub fn silent(label: impl Into<String>) -> Progress {
        Progress::new(label, None, false)
    }

    pub fn add(&mut self, bytes: u64) {
        self.done += bytes;
        self.draw(false);
    }

    pub fn set_total(&mut self, total: Option<u64>) {
        self.total = total;
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Erases the line. Called before any other output so that a message never
    /// lands on top of a half-drawn bar.
    pub fn clear(&mut self) {
        if !self.enabled || !self.drawn {
            return;
        }
        let mut stderr = std::io::stderr();
        let width = line_width(&self.render(BAR_WIDTH));
        let _ = write!(stderr, "\r{}\r", " ".repeat(width));
        let _ = stderr.flush();
        self.drawn = false;
    }

    /// Erases the line and reports the final state once, on stderr.
    pub fn finish(&mut self, message: &str) {
        self.clear();
        eprintln!("{message}");
    }

    fn draw(&mut self, force: bool) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if !force {
            if let Some(last) = self.last_draw {
                if now.duration_since(last) < FRAME {
                    return;
                }
            }
        }
        self.last_draw = Some(now);

        let line = self.fitted();
        let mut stderr = std::io::stderr();
        let _ = write!(stderr, "\r{line}");
        let _ = stderr.flush();
        self.drawn = true;
    }

    /// The line, narrowed to the terminal when the terminal said how wide it
    /// is. The bar gives up its width first: the numbers are what a narrow
    /// window cannot do without.
    fn fitted(&self) -> String {
        let width = terminal_width();
        let full = self.render(BAR_WIDTH);
        if line_width(&full) <= width {
            return full;
        }
        let over = line_width(&full) - width;
        cut(self.render(bar_width_for(over)), width)
    }

    fn render(&self, bar_width: usize) -> String {
        let rate = {
            let seconds = self.started.elapsed().as_secs_f64();
            if seconds > 0.2 {
                self.done as f64 / seconds
            } else {
                0.0
            }
        };
        match self.total {
            Some(total) if total > 0 => {
                let ratio = (self.done as f64 / total as f64).clamp(0.0, 1.0);
                let filled = (ratio * bar_width as f64).round() as usize;
                let bar: String = "█".repeat(filled) + &"░".repeat(bar_width - filled);
                let left = match rate {
                    r if r > 1.0 => {
                        let remaining = (total.saturating_sub(self.done)) as f64 / r;
                        format!("  eta {}", clock(remaining))
                    }
                    _ => String::new(),
                };
                format!(
                    "{} {} {:>5.1}%  {}/{}{}",
                    self.label,
                    bar,
                    ratio * 100.0,
                    human_bytes(self.done),
                    human_bytes(total),
                    left
                )
            }
            _ => format!(
                "{} {}  {}  {}",
                self.label,
                spinner(self.started.elapsed()),
                human_bytes(self.done),
                rate_text(rate)
            ),
        }
    }
}

fn spinner(elapsed: Duration) -> &'static str {
    const FRAMES: [&str; 4] = ["◐", "◓", "◑", "◒"];
    FRAMES[(elapsed.as_millis() / 120) as usize % FRAMES.len()]
}

/// How much of the bar survives a line this much too long.
fn bar_width_for(over: usize) -> usize {
    BAR_WIDTH.saturating_sub(over).max(MIN_BAR_WIDTH)
}

/// The line, cut to a character count. Every glyph here is one cell wide, so
/// the count is the width.
fn cut(line: String, width: usize) -> String {
    if line_width(&line) <= width {
        line
    } else {
        line.chars().take(width).collect()
    }
}

/// How many columns the terminal claims, when it says. `COLUMNS` is the one
/// source a program with nothing but `std` can read, and an unset one means
/// "as wide as the line needs".
fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|width| *width >= MIN_TERMINAL)
        .unwrap_or(usize::MAX)
}

fn line_width(text: &str) -> usize {
    // Every glyph used here is one cell wide, so counting characters is enough
    // and avoids pulling in a Unicode width table for a progress bar.
    text.chars().count()
}

pub fn rate_text(bytes_per_second: f64) -> String {
    if bytes_per_second < 1.0 {
        String::new()
    } else {
        format!("{}/s", human_bytes(bytes_per_second as u64))
    }
}

fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// Binary units with one decimal above a kibibyte: the same shape `ls -lh`
/// prints, so the number can be compared with one without arithmetic.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `1.2 s`, `2:03`, `1:02:03`. Used for the run summary and for `eta`.
pub fn human_duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 60.0 {
        format!("{seconds:.1} s")
    } else {
        let total = seconds as u64;
        let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
        if hours > 0 {
            format!("{hours}:{minutes:02}:{seconds:02}")
        } else {
            format!("{minutes}:{seconds:02}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_use_binary_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn durations_read_like_a_clock() {
        assert_eq!(human_duration(Duration::from_millis(1200)), "1.2 s");
        assert_eq!(human_duration(Duration::from_secs(63)), "1:03");
        assert_eq!(human_duration(Duration::from_secs(3723)), "1:02:03");
    }

    #[test]
    fn a_silent_progress_never_draws() {
        let mut progress = Progress::silent("test");
        progress.add(1024);
        progress.set_total(Some(2048));
        progress.clear();
        progress.finish("");
        assert_eq!(progress.done, 1024);
    }

    #[test]
    fn rates_are_empty_below_one_byte() {
        assert_eq!(rate_text(0.5), "");
        assert_eq!(rate_text(2048.0), "2.0 KiB/s");
    }
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    #[test]
    fn a_line_that_fits_is_left_alone() {
        assert_eq!(cut("abc".to_string(), 10), "abc");
        assert_eq!(cut("abcdef".to_string(), 3), "abc");
        assert_eq!(cut("abcdef".to_string(), 0), "");
    }

    #[test]
    fn the_bar_gives_up_its_width_before_the_numbers_do() {
        assert_eq!(bar_width_for(0), BAR_WIDTH);
        assert_eq!(bar_width_for(4), BAR_WIDTH - 4);
        assert_eq!(bar_width_for(BAR_WIDTH), MIN_BAR_WIDTH);
        assert_eq!(bar_width_for(1000), MIN_BAR_WIDTH);
    }
}
