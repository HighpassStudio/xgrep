use crate::cli::Args;
use crate::search::SearchResult;
use std::io::{self, Write as IoWrite};
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};

pub struct Writer {
    use_color: bool,
}

impl Writer {
    pub fn new(args: &Args) -> Self {
        let use_color = !args.no_color && atty_stdout();
        Writer { use_color }
    }

    pub fn write_matches(&self, result: &SearchResult, args: &Args) {
        if self.use_color {
            self.write_matches_color(result, args);
        } else {
            self.write_matches_plain(result, args);
        }
    }

    fn write_matches_plain(&self, result: &SearchResult, args: &Args) {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let show_file = args.show_filename();

        // Merge matches and context lines, sorted by line number
        let mut all_lines: Vec<(usize, &str, bool)> = Vec::new();
        for m in &result.matches {
            all_lines.push((m.line_number, &m.line, true));
        }
        for (ln, line) in &result.context_lines {
            all_lines.push((*ln, line, false));
        }
        all_lines.sort_by_key(|(ln, _, _)| *ln);
        all_lines.dedup_by_key(|(ln, _, _)| *ln);

        let mut prev_ln = 0usize;

        for (ln, line, is_match) in &all_lines {
            // Group separator
            if args.has_context() && prev_ln > 0 && *ln > prev_ln + 1 {
                let _ = writeln!(out, "--");
            }
            prev_ln = *ln;

            if *is_match && args.only_matching {
                // -o mode: print only matched portions
                let matcher_match = result
                    .matches
                    .iter()
                    .find(|m| m.line_number == *ln)
                    .unwrap();
                let matched_text = &line[matcher_match.match_start..matcher_match.match_end];
                if show_file {
                    let _ = write!(out, "{}:", result.path);
                }
                if args.line_number {
                    let _ = write!(out, "{}:", ln);
                }
                let _ = writeln!(out, "{}", matched_text);
            } else {
                if show_file {
                    let _ = write!(out, "{}:", result.path);
                }
                if args.line_number {
                    let sep = if *is_match { ':' } else { '-' };
                    let _ = write!(out, "{}{}", ln, sep);
                }
                let _ = writeln!(out, "{}", line);
            }
        }
    }

    fn write_matches_color(&self, result: &SearchResult, args: &Args) {
        let mut stdout = StandardStream::stdout(ColorChoice::Always);
        let show_file = args.show_filename();

        let mut all_lines: Vec<(usize, &str, bool)> = Vec::new();
        for m in &result.matches {
            all_lines.push((m.line_number, &m.line, true));
        }
        for (ln, line) in &result.context_lines {
            all_lines.push((*ln, line, false));
        }
        all_lines.sort_by_key(|(ln, _, _)| *ln);
        all_lines.dedup_by_key(|(ln, _, _)| *ln);

        let mut prev_ln = 0usize;

        for (ln, line, is_match) in &all_lines {
            if args.has_context() && prev_ln > 0 && *ln > prev_ln + 1 {
                let _ = stdout.set_color(ColorSpec::new().set_fg(Some(Color::Cyan)));
                let _ = writeln!(stdout, "--");
                let _ = stdout.reset();
            }
            prev_ln = *ln;

            // Filename
            if show_file {
                let _ = stdout.set_color(ColorSpec::new().set_fg(Some(Color::Magenta)));
                let _ = write!(stdout, "{}", result.path);
                let _ = stdout.set_color(ColorSpec::new().set_fg(Some(Color::Cyan)));
                let _ = write!(stdout, ":");
                let _ = stdout.reset();
            }

            // Line number
            if args.line_number {
                let _ = stdout.set_color(ColorSpec::new().set_fg(Some(Color::Green)));
                let _ = write!(stdout, "{}", ln);
                let _ = stdout.set_color(ColorSpec::new().set_fg(Some(Color::Cyan)));
                let sep = if *is_match { ':' } else { '-' };
                let _ = write!(stdout, "{}", sep);
                let _ = stdout.reset();
            }

            if *is_match {
                let m = result
                    .matches
                    .iter()
                    .find(|m| m.line_number == *ln)
                    .unwrap();

                if args.only_matching {
                    let matched = &line[m.match_start..m.match_end];
                    let _ = stdout
                        .set_color(ColorSpec::new().set_fg(Some(Color::Red)).set_bold(true));
                    let _ = write!(stdout, "{}", matched);
                    let _ = stdout.reset();
                    let _ = writeln!(stdout);
                } else {
                    // Before match
                    let _ = write!(stdout, "{}", &line[..m.match_start]);
                    // Match highlight
                    let _ = stdout
                        .set_color(ColorSpec::new().set_fg(Some(Color::Red)).set_bold(true));
                    let _ = write!(stdout, "{}", &line[m.match_start..m.match_end]);
                    let _ = stdout.reset();
                    // After match
                    let _ = writeln!(stdout, "{}", &line[m.match_end..]);
                }
            } else {
                let _ = writeln!(stdout, "{}", line);
            }
        }
    }

    pub fn write_count(&self, path: &str, count: usize) {
        if self.use_color {
            let mut stdout = StandardStream::stdout(ColorChoice::Always);
            let _ = stdout.set_color(ColorSpec::new().set_fg(Some(Color::Magenta)));
            let _ = write!(stdout, "{}", path);
            let _ = stdout.set_color(ColorSpec::new().set_fg(Some(Color::Cyan)));
            let _ = write!(stdout, ":");
            let _ = stdout.reset();
            let _ = writeln!(stdout, "{}", count);
        } else {
            println!("{}:{}", path, count);
        }
    }

    pub fn write_total_count(&self, count: u64) {
        println!("{}", count);
    }

    pub fn write_filename(&self, path: &str) {
        if self.use_color {
            let mut stdout = StandardStream::stdout(ColorChoice::Always);
            let _ = stdout.set_color(ColorSpec::new().set_fg(Some(Color::Magenta)));
            let _ = writeln!(stdout, "{}", path);
            let _ = stdout.reset();
        } else {
            println!("{}", path);
        }
    }
}

fn atty_stdout() -> bool {
    // Simple TTY detection
    #[cfg(unix)]
    {
        unsafe { libc::isatty(1) != 0 }
    }
    #[cfg(not(unix))]
    {
        // On Windows, default to color if not piped
        true
    }
}
