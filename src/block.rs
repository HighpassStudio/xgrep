/// Block-level search engine.
///
/// Two prefilter strategies:
///
/// 1. **SIMD literal precheck** (first-pass, zero build cost):
///    Use memchr to scan each 64KB block for the query literal.
///    If the literal isn't in the block, skip it entirely.
///    Extremely fast — no bloom build overhead.
///
/// 2. **Cached bloom index** (repeated queries):
///    Pre-built token-level bloom filters loaded from disk.
///    Skips blocks without even reading them from the file.
///    Only used when index cache exists.

use crate::bloom;
use crate::cli::Args;
use crate::discover::{FileEntry, FileFormat};
use crate::matcher::{Match, Matcher};
use crate::search::SearchResult;
use anyhow::Result;
use flate2::read::GzDecoder;
use memchr::memmem;
use std::fs::File;
use std::io::Read;

const BLOCK_SIZE: usize = 64 * 1024; // 64KB

pub struct BlockSearchStats {
    pub total_blocks: usize,
    pub skipped_blocks: usize,
    pub searched_blocks: usize,
}

/// Block-skip search entry point.
pub fn block_search_file(
    file: &FileEntry,
    matcher: &Matcher,
    args: &Args,
    literals: &Option<Vec<u8>>,
) -> Result<(SearchResult, BlockSearchStats)> {
    match file.format {
        FileFormat::PlainText => block_search_plain(file, matcher, args, literals),
        FileFormat::Gzip => block_search_gzip(file, matcher, args, literals),
    }
}

fn block_search_plain(
    file: &FileEntry,
    matcher: &Matcher,
    args: &Args,
    literals: &Option<Vec<u8>>,
) -> Result<(SearchResult, BlockSearchStats)> {
    let data = std::fs::read(&file.path)?;
    let path_str = file.path.to_string_lossy().to_string();
    block_search_bytes(&data, &path_str, matcher, args, literals)
}

fn block_search_gzip(
    file: &FileEntry,
    matcher: &Matcher,
    args: &Args,
    literals: &Option<Vec<u8>>,
) -> Result<(SearchResult, BlockSearchStats)> {
    let f = File::open(&file.path)?;
    let mut decoder = GzDecoder::new(f);
    let mut data = Vec::new();
    decoder.read_to_end(&mut data)?;
    let path_str = file.path.to_string_lossy().to_string();
    block_search_bytes(&data, &path_str, matcher, args, literals)
}

/// Core block-skip search using SIMD literal precheck.
fn block_search_bytes(
    data: &[u8],
    path: &str,
    matcher: &Matcher,
    args: &Args,
    literals: &Option<Vec<u8>>,
) -> Result<(SearchResult, BlockSearchStats)> {
    let num_blocks = (data.len() + BLOCK_SIZE - 1) / BLOCK_SIZE;
    let mut skipped = 0usize;

    // Phase 1: SIMD literal precheck per block — zero build cost
    let mut candidate_set = vec![true; num_blocks];

    if let Some(lit) = literals.as_deref() {
        let needle = if args.ignore_case {
            lit.to_ascii_lowercase()
        } else {
            lit.to_vec()
        };
        let finder = memmem::Finder::new(&needle);

        for block_idx in 0..num_blocks {
            let start = block_idx * BLOCK_SIZE;
            let end = std::cmp::min(start + BLOCK_SIZE, data.len());
            let block_data = &data[start..end];

            let haystack = if args.ignore_case {
                // For case-insensitive, we need to lowercase the block
                // This is more expensive but still cheaper than bloom build
                let lower: Vec<u8> = block_data.iter().map(|b| b.to_ascii_lowercase()).collect();
                finder.find(&lower).is_some()
            } else {
                finder.find(block_data).is_some()
            };

            if !haystack {
                candidate_set[block_idx] = false;
                skipped += 1;
            }
        }
    }

    // Phase 2: Search only candidate blocks
    let needs_context = args.has_context();
    let before = args.before_context;
    let after = args.after_context;
    let max_count = args.max_count.unwrap_or(usize::MAX);
    let count_only = (args.count || args.files_with_matches || args.quiet) && !needs_context;

    let mut pos = 0usize;

    if count_only {
        let mut count = 0usize;
        while pos < data.len() {
            let line_end = memchr::memchr(b'\n', &data[pos..])
                .map(|i| pos + i)
                .unwrap_or(data.len());

            let block_idx = pos / BLOCK_SIZE;
            if candidate_set[block_idx] {
                let line_end_trim = if line_end > pos && data[line_end - 1] == b'\r' {
                    line_end - 1
                } else {
                    line_end
                };
                let line = String::from_utf8_lossy(&data[pos..line_end_trim]);
                if matcher.find_in_line(&line).is_some() {
                    count += 1;
                    if count >= max_count || args.quiet {
                        break;
                    }
                }
            }

            pos = line_end + 1;
        }

        let dummy_matches: Vec<Match> = (0..count)
            .map(|_| Match {
                line_number: 0,
                line: String::new(),
                match_start: 0,
                match_end: 0,
            })
            .collect();

        return Ok((
            SearchResult {
                path: path.to_string(),
                matches: dummy_matches,
                context_lines: Vec::new(),
            },
            BlockSearchStats {
                total_blocks: num_blocks,
                skipped_blocks: skipped,
                searched_blocks: num_blocks - skipped,
            },
        ));
    }

    // Full output path
    let mut matches: Vec<Match> = Vec::new();
    let mut context_lines: Vec<(usize, String)> = Vec::new();
    let mut line_num = 0usize;
    let mut before_buf: Vec<(usize, String)> = Vec::with_capacity(before + 1);
    let mut after_remaining = 0usize;

    while pos < data.len() && matches.len() < max_count {
        let line_end = memchr::memchr(b'\n', &data[pos..])
            .map(|i| pos + i)
            .unwrap_or(data.len());

        line_num += 1;

        let block_idx = pos / BLOCK_SIZE;
        let in_candidate = candidate_set[block_idx];

        let line_end_trim = if line_end > pos && data[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };

        if in_candidate {
            let line_str = String::from_utf8_lossy(&data[pos..line_end_trim]);

            if let Some((start, end)) = matcher.find_in_line(&line_str) {
                if needs_context {
                    for ctx in before_buf.drain(..) {
                        context_lines.push(ctx);
                    }
                }

                matches.push(Match {
                    line_number: line_num,
                    line: line_str.into_owned(),
                    match_start: start,
                    match_end: end,
                });

                after_remaining = after;
            } else if needs_context {
                if after_remaining > 0 {
                    context_lines.push((line_num, line_str.into_owned()));
                    after_remaining -= 1;
                } else if before > 0 {
                    if before_buf.len() >= before {
                        before_buf.remove(0);
                    }
                    before_buf.push((line_num, line_str.into_owned()));
                }
            }
        } else if needs_context {
            if after_remaining > 0 {
                let line_str = String::from_utf8_lossy(&data[pos..line_end_trim]);
                context_lines.push((line_num, line_str.into_owned()));
                after_remaining -= 1;
            } else if before > 0 {
                let line_str = String::from_utf8_lossy(&data[pos..line_end_trim]);
                if before_buf.len() >= before {
                    before_buf.remove(0);
                }
                before_buf.push((line_num, line_str.into_owned()));
            }
        }

        if args.quiet && !matches.is_empty() {
            break;
        }

        pos = line_end + 1;
    }

    Ok((
        SearchResult {
            path: path.to_string(),
            matches,
            context_lines,
        },
        BlockSearchStats {
            total_blocks: num_blocks,
            skipped_blocks: skipped,
            searched_blocks: num_blocks - skipped,
        },
    ))
}
