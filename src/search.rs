use crate::cli::Args;
use crate::discover::{FileEntry, FileFormat};
use crate::matcher::{Match, Matcher};
use anyhow::Result;
use flate2::read::GzDecoder;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};

pub struct SearchResult {
    pub path: String,
    pub matches: Vec<Match>,
    pub context_lines: Vec<(usize, String)>,
}

pub fn search_file(file: &FileEntry, matcher: &Matcher, args: &Args) -> Result<SearchResult> {
    let path_str = file.path.to_string_lossy().to_string();

    // Fast path: count-only or files-with-matches on plain text — avoid storing lines
    if (args.count || args.files_with_matches || args.quiet) && !args.has_context() {
        return match file.format {
            FileFormat::PlainText => count_matches_plain(&file.path, &path_str, matcher, args),
            FileFormat::Gzip => {
                let f = File::open(&file.path)?;
                count_matches_reader(Box::new(GzDecoder::new(f)), &path_str, matcher, args)
            }
        };
    }

    let reader: Box<dyn Read> = match file.format {
        FileFormat::PlainText => Box::new(File::open(&file.path)?),
        FileFormat::Gzip => {
            let f = File::open(&file.path)?;
            Box::new(GzDecoder::new(f))
        }
    };

    search_reader(reader, &path_str, matcher, args)
}

/// Fast path: just count matches without allocating line strings.
fn count_matches_plain(
    path: &std::path::Path,
    path_str: &str,
    matcher: &Matcher,
    args: &Args,
) -> Result<SearchResult> {
    let f = File::open(path)?;
    count_matches_reader(Box::new(f), path_str, matcher, args)
}

fn count_matches_reader(
    reader: Box<dyn Read>,
    path_str: &str,
    matcher: &Matcher,
    args: &Args,
) -> Result<SearchResult> {
    let buf = BufReader::with_capacity(256 * 1024, reader);
    let max_count = args.max_count.unwrap_or(usize::MAX);
    let mut count = 0usize;
    let mut line_buf = String::with_capacity(512);

    let mut reader = buf;
    loop {
        line_buf.clear();
        let bytes_read = reader.read_line(&mut line_buf)?;
        if bytes_read == 0 {
            break;
        }

        // Trim trailing newline for matching
        let line = line_buf.trim_end_matches('\n').trim_end_matches('\r');

        if matcher.find_in_line(line).is_some() {
            count += 1;
            if count >= max_count || args.quiet {
                break;
            }
        }
    }

    // Create dummy matches for count
    let matches: Vec<Match> = (0..count)
        .map(|_| Match {
            line_number: 0,
            line: String::new(),
            match_start: 0,
            match_end: 0,
        })
        .collect();

    Ok(SearchResult {
        path: path_str.to_string(),
        matches,
        context_lines: Vec::new(),
    })
}

fn search_reader(
    reader: Box<dyn Read>,
    path: &str,
    matcher: &Matcher,
    args: &Args,
) -> Result<SearchResult> {
    let buf = BufReader::with_capacity(256 * 1024, reader);
    let mut matches = Vec::new();
    let mut context_lines: Vec<(usize, String)> = Vec::new();

    let needs_context = args.has_context();
    let before = args.before_context;
    let after = args.after_context;

    let mut before_buf: Vec<(usize, String)> = Vec::with_capacity(before + 1);
    let mut after_remaining = 0usize;
    let max_count = args.max_count.unwrap_or(usize::MAX);

    let mut line_buf = String::with_capacity(512);
    let mut line_num = 0usize;
    let mut reader = buf;

    loop {
        line_buf.clear();
        let bytes_read = reader.read_line(&mut line_buf)?;
        if bytes_read == 0 {
            break;
        }
        line_num += 1;

        let line = line_buf.trim_end_matches('\n').trim_end_matches('\r');

        if let Some((start, end)) = matcher.find_in_line(line) {
            if matches.len() >= max_count {
                break;
            }

            if needs_context {
                for ctx in before_buf.drain(..) {
                    context_lines.push(ctx);
                }
            }

            matches.push(Match {
                line_number: line_num,
                line: line.to_owned(),
                match_start: start,
                match_end: end,
            });

            after_remaining = after;
        } else if needs_context {
            if after_remaining > 0 {
                context_lines.push((line_num, line.to_owned()));
                after_remaining -= 1;
            } else if before > 0 {
                if before_buf.len() >= before {
                    before_buf.remove(0);
                }
                before_buf.push((line_num, line.to_owned()));
            }
        }

        if args.quiet && !matches.is_empty() {
            break;
        }
    }

    Ok(SearchResult {
        path: path.to_string(),
        matches,
        context_lines,
    })
}
