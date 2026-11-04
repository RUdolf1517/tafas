//! Colour, and the rules that decide whether it is used.
//!
//! The precedence is the one the wider ecosystem already implements, because a
//! tool that invents its own would be the only one misbehaving in a pipeline:
//!
//! 1. `--color always|never` wins over everything;
//! 2. `--no-color` is the same as `--color never`;
//! 3. `NO_COLOR` (non-empty) disables, `CLICOLOR_FORCE` (non-empty, not `0`)
//!    enables, `CLICOLOR=0` disables, `TERM=dumb` disables;
//! 4. otherwise colour is used only when the stream is a terminal.
//!
//! Nothing is coloured on a pipe, so `tafas … | xargs …` receives clean text.

use std::io::IsTerminal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Auto,
    Always,
    Never,
}

impl Choice {
    pub fn parse(text: &str) -> Result<Choice, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Choice::Auto),
            "always" | "yes" | "force" => Ok(Choice::Always),
            "never" | "no" | "none" => Ok(Choice::Never),
            other => Err(format!(
                "unknown colour setting '{other}': expected auto, always or never"
            )),
        }
    }
}

pub const BOLD: &str = "1";
pub const DIM: &str = "2";
pub const RED: &str = "31";
pub const GREEN: &str = "32";
pub const YELLOW: &str = "33";
pub const CYAN: &str = "36";

#[derive(Debug, Clone, Copy)]
pub struct Pen {
    enabled: bool,
}

impl Pen {
    pub fn new(choice: Choice, is_terminal: bool) -> Pen {
        let enabled = match choice {
            Choice::Never => false,
            Choice::Always => true,
            Choice::Auto => is_terminal && environment_allows(),
        };
        Pen { enabled }
    }

    pub fn for_stdout(choice: Choice) -> Pen {
        Pen::new(choice, std::io::stdout().is_terminal())
    }

    pub fn for_stderr(choice: Choice) -> Pen {
        Pen::new(choice, std::io::stderr().is_terminal())
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn paint(&self, code: &str, text: &str) -> String {
        if self.enabled {
            format!("\u{1b}[{code}m{text}\u{1b}[0m")
        } else {
            text.to_string()
        }
    }
}

fn environment_allows() -> bool {
    if std::env::var_os("NO_COLOR").map(|v| !v.is_empty()).unwrap_or(false) {
        return false;
    }
    if let Some(force) = std::env::var_os("CLICOLOR_FORCE") {
        let force = force.to_string_lossy();
        if !force.is_empty() && force != "0" {
            return true;
        }
    }
    if let Some(value) = std::env::var_os("CLICOLOR") {
        if value == "0" {
            return false;
        }
    }
    match std::env::var_os("TERM") {
        Some(term) => term != "dumb",
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_choices_ignore_the_terminal() {
        assert!(!Pen::new(Choice::Never, true).enabled());
        assert!(Pen::new(Choice::Always, false).enabled());
        assert!(!Pen::new(Choice::Auto, false).enabled());
    }

    #[test]
    fn paints_only_when_enabled() {
        assert_eq!(Pen::new(Choice::Never, true).paint(BOLD, "x"), "x");
        assert_eq!(
            Pen::new(Choice::Always, false).paint(BOLD, "x"),
            "\u{1b}[1mx\u{1b}[0m"
        );
    }

    #[test]
    fn parses_the_three_settings() {
        assert_eq!(Choice::parse("auto").unwrap(), Choice::Auto);
        assert_eq!(Choice::parse("always").unwrap(), Choice::Always);
        assert_eq!(Choice::parse("never").unwrap(), Choice::Never);
        assert!(Choice::parse("rainbow").is_err());
    }
}
