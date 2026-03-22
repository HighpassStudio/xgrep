use crate::cli::Args;
use anyhow::{Context, Result};
use memchr::memmem;
use regex::Regex;

pub enum Matcher {
    Fixed(FixedMatcher),
    Regex(RegexMatcher),
}

pub struct FixedMatcher {
    pattern: String,
    ignore_case: bool,
    lower_pattern: String,
}

pub struct RegexMatcher {
    re: Regex,
}

pub fn build(args: &Args) -> Result<Matcher> {
    if args.fixed_strings {
        let lower = if args.ignore_case {
            args.pattern.to_lowercase()
        } else {
            args.pattern.clone()
        };
        Ok(Matcher::Fixed(FixedMatcher {
            pattern: args.pattern.clone(),
            ignore_case: args.ignore_case,
            lower_pattern: lower,
        }))
    } else {
        let mut pat = args.pattern.clone();
        if args.ignore_case {
            pat = format!("(?i){}", pat);
        }
        let re = Regex::new(&pat).context("invalid regex pattern")?;
        Ok(Matcher::Regex(RegexMatcher { re }))
    }
}

#[derive(Debug, Clone)]
pub struct Match {
    pub line_number: usize,
    pub line: String,
    pub match_start: usize,
    pub match_end: usize,
}

impl Matcher {
    /// Search a single line, return match info if found.
    pub fn find_in_line(&self, line: &str) -> Option<(usize, usize)> {
        match self {
            Matcher::Fixed(f) => {
                if f.ignore_case {
                    let lower_line = line.to_lowercase();
                    let pos = memmem::find(lower_line.as_bytes(), f.lower_pattern.as_bytes())?;
                    Some((pos, pos + f.pattern.len()))
                } else {
                    let pos = memmem::find(line.as_bytes(), f.pattern.as_bytes())?;
                    Some((pos, pos + f.pattern.len()))
                }
            }
            Matcher::Regex(r) => {
                let m = r.re.find(line)?;
                Some((m.start(), m.end()))
            }
        }
    }

    /// Find all matches in a line (for -o mode).
    pub fn find_all_in_line(&self, line: &str) -> Vec<(usize, usize)> {
        match self {
            Matcher::Fixed(f) => {
                let (haystack, needle) = if f.ignore_case {
                    (line.to_lowercase(), f.lower_pattern.as_str().to_owned())
                } else {
                    (line.to_owned(), f.pattern.clone())
                };
                memmem::find_iter(haystack.as_bytes(), needle.as_bytes())
                    .map(|pos| (pos, pos + f.pattern.len()))
                    .collect()
            }
            Matcher::Regex(r) => r.re.find_iter(line).map(|m| (m.start(), m.end())).collect(),
        }
    }
}
