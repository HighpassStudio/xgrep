mod block;
mod bloom;
mod cli;
mod discover;
mod index;
mod matcher;
mod output;
mod query;
mod search;

use anyhow::Result;
use cli::Args;
use rayon::prelude::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

/// Search a single file across all four code paths, converting both errors and
/// panics into a logged warning + `None` so one bad file (locked by another
/// process, partially written, mid-rotation, etc.) can't abort the whole
/// search. Fixes v0.1.0 PermissionDenied panic on actively-written trees.
fn try_search_file(
    file: &discover::FileEntry,
    consolidated_indexes: &std::collections::HashMap<std::path::PathBuf, index::ConsolidatedIndex>,
    matcher: &matcher::Matcher,
    args: &Args,
    literals: &Option<Vec<u8>>,
    json_filters: &Option<Vec<query::JsonFilter>>,
) -> Option<(search::SearchResult, block::BlockSearchStats)> {
    let path_display = file.path.display().to_string();

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let dir = file
            .path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();

        // Path 1: consolidated index (single mmap, 2 file opens total)
        if let Some(idx) = consolidated_indexes.get(&dir) {
            return index::consolidated_search(idx, file, matcher, args, literals, json_filters);
        }

        // Path 2: per-file cached index
        if index::has_cached_index(file) {
            return match index::cached_search(file, matcher, args, literals, json_filters) {
                Ok(r) => Some(r).filter(|(r, _)| !r.matches.is_empty()),
                Err(e) => {
                    eprintln!("xgrep: {}: {}", path_display, e);
                    None
                }
            };
        }

        // Path 3: SIMD block-skip (no cache)
        if literals.is_some() || json_filters.is_some() {
            return match block::block_search_file(file, matcher, args, literals, json_filters) {
                Ok(r) => Some(r).filter(|(r, _)| !r.matches.is_empty()),
                Err(e) => {
                    eprintln!("xgrep: {}: {}", path_display, e);
                    None
                }
            };
        }

        // Path 4: fallback line-by-line
        match search::search_file(file, matcher, args) {
            Ok(r) if !r.matches.is_empty() => Some((
                r,
                block::BlockSearchStats {
                    total_blocks: 0,
                    skipped_blocks: 0,
                    searched_blocks: 0,
                },
            )),
            Ok(_) => None,
            Err(e) => {
                eprintln!("xgrep: {}: {}", path_display, e);
                None
            }
        }
    }));

    match outcome {
        Ok(opt) => opt,
        Err(_) => {
            // Third-party crate panicked on this file (flate2 on a
            // truncated gzip, mmap on a vanishing file, etc.). Skip it.
            eprintln!("xgrep: {}: skipped (internal panic)", path_display);
            None
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse_args()?;
    let files = discover::find_files(&args)?;

    if files.is_empty() {
        return Ok(());
    }

    // --build-index: create consolidated sidecar cache per directory
    if args.build_index {
        index::build_consolidated_index(&files, args.json)?;
        return Ok(());
    }

    // Parse JSON filters if -j mode
    let json_filters = if args.json {
        match query::parse_json_query(&args.pattern) {
            Ok(filters) => Some(filters),
            Err(e) => {
                eprintln!("xgrep: invalid JSON filter: {}", e);
                std::process::exit(2);
            }
        }
    } else {
        None
    };

    let matcher = matcher::build(&args)?;
    let writer = output::Writer::new(&args);

    // For JSON mode, extract the most selective value as literal for SIMD precheck
    let literals = if args.json {
        json_filters.as_ref().and_then(|filters| {
            filters
                .iter()
                .map(|f| &f.value)
                .filter(|v| v.len() >= 3)
                .max_by_key(|v| v.len())
                .map(|v| v.as_bytes().to_vec())
        })
    } else {
        bloom::extract_literals(&args.pattern, args.fixed_strings)
    };

    // Check for consolidated index in each source directory
    let mut dirs_with_index: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    let mut consolidated_indexes: std::collections::HashMap<
        std::path::PathBuf,
        index::ConsolidatedIndex,
    > = std::collections::HashMap::new();

    for file in &files {
        let dir = file
            .path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        if !dirs_with_index.contains(&dir) && index::has_consolidated_index(&dir) {
            dirs_with_index.insert(dir.clone());
            if let Ok(idx) = index::load_consolidated_index(&dir) {
                consolidated_indexes.insert(dir, idx);
            }
        }
    }

    let results: Vec<_> = files
        .par_iter()
        .filter_map(|file| {
            try_search_file(
                file,
                &consolidated_indexes,
                &matcher,
                &args,
                &literals,
                &json_filters,
            )
        })
        .collect();

    // Output
    let mut count = 0u64;
    let mut total_blocks = 0usize;
    let mut total_skipped = 0usize;

    for (result, stats) in &results {
        total_blocks += stats.total_blocks;
        total_skipped += stats.skipped_blocks;

        if args.quiet && !result.matches.is_empty() {
            std::process::exit(0);
        }
        if args.count {
            count += result.matches.len() as u64;
            if args.show_filename() {
                writer.write_count(&result.path, result.matches.len());
            }
        } else if args.files_with_matches {
            writer.write_filename(&result.path);
        } else {
            writer.write_matches(result, &args);
        }
    }

    if args.count && !args.show_filename() {
        writer.write_total_count(count);
    }

    if args.stats && total_blocks > 0 {
        eprintln!(
            "[xgrep] blocks: {} total, {} skipped ({}%), {} searched",
            total_blocks,
            total_skipped,
            if total_blocks > 0 {
                total_skipped * 100 / total_blocks
            } else {
                0
            },
            total_blocks - total_skipped,
        );
    }

    if results.is_empty() {
        std::process::exit(1);
    }

    Ok(())
}
