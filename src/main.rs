mod block;
mod bloom;
mod cli;
mod discover;
mod index;
mod matcher;
mod output;
mod search;

use anyhow::Result;
use cli::Args;
use rayon::prelude::*;
use std::path::Path;

fn main() -> Result<()> {
    let args = Args::parse_args()?;
    let files = discover::find_files(&args)?;

    if files.is_empty() {
        return Ok(());
    }

    // --build-index: create consolidated sidecar cache per directory
    if args.build_index {
        index::build_consolidated_index(&files)?;
        return Ok(());
    }

    let matcher = matcher::build(&args)?;
    let writer = output::Writer::new(&args);
    let literals = bloom::extract_literals(&args.pattern, args.fixed_strings);

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
            let dir = file
                .path
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf();

            // Path 1: consolidated index (single mmap, 2 file opens total)
            if let Some(idx) = consolidated_indexes.get(&dir) {
                return index::consolidated_search(idx, file, &matcher, &args, &literals);
            }

            // Path 2: per-file cached index
            if index::has_cached_index(file) {
                return index::cached_search(file, &matcher, &args, &literals)
                    .ok()
                    .filter(|(r, _)| !r.matches.is_empty());
            }

            // Path 3: SIMD block-skip (no cache)
            if literals.is_some() {
                return block::block_search_file(file, &matcher, &args, &literals)
                    .ok()
                    .filter(|(r, _)| !r.matches.is_empty());
            }

            // Path 4: fallback line-by-line
            search::search_file(file, &matcher, &args)
                .ok()
                .filter(|r| !r.matches.is_empty())
                .map(|r| {
                    (
                        r,
                        block::BlockSearchStats {
                            total_blocks: 0,
                            skipped_blocks: 0,
                            searched_blocks: 0,
                        },
                    )
                })
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
