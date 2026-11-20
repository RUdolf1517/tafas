//! `--playlist-items`: which entries of a list to take.
//!
//! One grammar, used wherever a list of things has a position: `3`, `1-5`,
//! `2-` and any comma-separated combination of them, all 1-based, because the
//! user is counting and not indexing. It is parsed once, before the first
//! request, so a typo costs a usage error rather than a half-finished file.

use std::fmt;

use crate::error::{Error, Result};

/// A parsed item list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Items {
    /// The text the user typed, kept for messages: echoing it back is more
    /// useful than reprinting a normalised form they did not write.
    spec: String,
    /// Sorted, disjoint, inclusive 1-based ranges.
    ranges: Vec<(usize, usize)>,
}

impl Items {
    /// Parses `1-5,9`, `3`, `2-`, or any comma-separated mix.
    pub fn parse(spec: &str) -> Result<Items> {
        let text = spec.trim();
        if text.is_empty() {
            return Err(Error::Usage(
                "--playlist-items needs a list, such as 1-5,9".to_string(),
            ));
        }

        let mut ranges = Vec::new();
        for part in text.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(Error::Usage(format!(
                    "'{text}' is not an item list: the empty entry between two commas selects nothing"
                )));
            }
            let (start, end) = match part.split_once('-') {
                None => {
                    let index = number(part, text)?;
                    (index, index)
                }
                Some((start, end)) => {
                    let start = number(start.trim(), text)?;
                    let end = if end.trim().is_empty() {
                        usize::MAX
                    } else {
                        number(end.trim(), text)?
                    };
                    (start, end)
                }
            };
            if end < start {
                return Err(Error::Usage(format!(
                    "'{part}' runs backwards: {end} comes before {start}"
                )));
            }
            ranges.push((start, end));
        }

        ranges.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (start, end) in ranges {
            if let Some((_, previous_end)) = merged.last_mut() {
                if start <= previous_end.saturating_add(1) {
                    *previous_end = (*previous_end).max(end);
                    continue;
                }
            }
            merged.push((start, end));
        }

        Ok(Items {
            spec: text.to_string(),
            ranges: merged,
        })
    }

    /// Whether the entry at `index` (1-based) is selected.
    pub fn contains(&self, index: usize) -> bool {
        let next = self.ranges.partition_point(|(start, _)| *start <= index);
        next > 0 && index <= self.ranges[next - 1].1
    }

    /// How many entries of a list of `length` this selects.
    pub fn count_within(&self, length: usize) -> usize {
        self.ranges
            .iter()
            .take_while(|(start, _)| *start <= length)
            .map(|(start, end)| (*end).min(length) - *start + 1)
            .sum()
    }
}

impl fmt::Display for Items {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.spec)
    }
}

fn number(text: &str, spec: &str) -> Result<usize> {
    match text.parse::<usize>() {
        Ok(value) if value >= 1 => Ok(value),
        Ok(_) => Err(Error::Usage(format!(
            "'{spec}' counts from one: an item is never 0"
        ))),
        Err(_) => Err(Error::Usage(format!(
            "'{text}' is not an item number in '{spec}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(spec: &str) -> Items {
        Items::parse(spec).expect("a valid item list")
    }

    #[test]
    fn a_single_item_selects_one() {
        let list = items("3");
        assert!(list.contains(3));
        assert!(!list.contains(2));
        assert!(!list.contains(4));
    }

    #[test]
    fn ranges_are_inclusive_at_both_ends() {
        let list = items("1-3");
        assert_eq!(
            (1..=5)
                .filter(|index| list.contains(*index))
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn an_open_range_runs_to_the_end() {
        let list = items("4-");
        assert!(!list.contains(3));
        assert!(list.contains(4));
        assert!(list.contains(10_000));
        assert_eq!(list.count_within(usize::MAX), usize::MAX - 3);
    }

    #[test]
    fn a_mix_is_the_union() {
        let list = items("8-9,5,1-2,2,9");
        let taken: Vec<usize> = (1..=10).filter(|index| list.contains(*index)).collect();
        assert_eq!(taken, vec![1, 2, 5, 8, 9]);
        assert_eq!(list.count_within(10), 5);
    }

    #[test]
    fn whitespace_is_not_a_syntax_error() {
        let list = items(" 1 - 2 , 4 ");
        assert!(list.contains(1));
        assert!(list.contains(2));
        assert!(list.contains(4));
        assert!(!list.contains(3));
    }

    #[test]
    fn mistakes_are_named() {
        assert!(Items::parse("").is_err(), "nothing to select");
        assert!(Items::parse("0").is_err(), "items count from one");
        assert!(Items::parse("1,,2").is_err(), "an empty entry");
        assert!(Items::parse("5-2").is_err(), "a backwards range");
        assert!(Items::parse("one").is_err(), "not a number");
        assert!(Items::parse("1-2-3").is_err(), "two dashes");
    }

    #[test]
    fn the_typed_text_is_what_messages_echo() {
        assert_eq!(items(" 1 - 2 ").to_string(), "1 - 2");
    }
}
