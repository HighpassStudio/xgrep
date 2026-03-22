/// Consolidated index cache for repeated queries.
///
/// Two modes:
/// 1. Per-file index (original): .xgrep/<filename>.xgi + .xgd
/// 2. Consolidated index: .xgrep/index.xgi + index.xgd (all files in one)
///
/// Consolidated format eliminates N file opens → 2 opens.
///
/// Consolidated .xgi format:
///   [8 bytes]  magic: "XGREP02\0"
///   [4 bytes]  file_count
///   [4 bytes]  block_size
///   [4 bytes]  bloom_size
///   [4 bytes]  reserved
///   -- File table (file_count entries) --
///   Per file:
///     [4 bytes]  path_len
///     [path_len bytes]  source path (relative)
///     [8 bytes]  source_size
///     [8 bytes]  source_mtime_ns
///     [8 bytes]  data_offset (byte offset into .xgd)
///     [8 bytes]  data_len (decompressed size)
///     [4 bytes]  block_count
///     [block_count * bloom_size bytes]  bloom filters
///
/// Consolidated .xgd format:
///   Concatenated decompressed content for all files, in file table order.

use crate::bloom;
use crate::block::BlockSearchStats;
use crate::cli::Args;
use crate::discover::FileEntry;
use crate::matcher::{Match, Matcher};
use crate::query::JsonFilter;
use crate::search::SearchResult;
use anyhow::{Context, Result};
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write, BufWriter};
use std::path::{Path, PathBuf};

const MAGIC_V2: &[u8; 8] = b"XGREP02\0";
const BLOCK_SIZE: usize = 64 * 1024;
const BLOOM_SIZE: usize = 4096;

/// Parsed file entry from consolidated index.
struct IndexedFile {
    source_size: u64,
    source_mtime: u64,
    data_offset: u64,
    data_len: u64,
    blooms: Vec<bloom::BloomFilter>,
}

/// Loaded consolidated index.
pub struct ConsolidatedIndex {
    files: HashMap<String, IndexedFile>,
    data_mmap: Mmap,
}

/// Build consolidated index for a set of files in a directory.
pub fn build_consolidated_index(files: &[FileEntry], json_mode: bool) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    // Group files by parent directory
    let mut by_dir: HashMap<PathBuf, Vec<&FileEntry>> = HashMap::new();
    for file in files {
        let dir = file.path.parent().unwrap_or(Path::new(".")).to_path_buf();
        by_dir.entry(dir).or_default().push(file);
    }

    for (dir, dir_files) in &by_dir {
        build_index_for_dir(dir, dir_files, json_mode)?;
    }

    Ok(())
}

fn build_index_for_dir(dir: &Path, files: &[&FileEntry], json_mode: bool) -> Result<()> {
    let cache_dir = dir.join(".xgrep");
    fs::create_dir_all(&cache_dir)?;

    let data_path = cache_dir.join("index.xgd");
    let index_path = cache_dir.join("index.xgi");

    let mut data_file = BufWriter::new(File::create(&data_path)?);
    let mut index_buf: Vec<u8> = Vec::new();

    // Header
    index_buf.extend_from_slice(MAGIC_V2);
    index_buf.extend_from_slice(&(files.len() as u32).to_le_bytes());
    index_buf.extend_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
    index_buf.extend_from_slice(&(BLOOM_SIZE as u32).to_le_bytes());
    index_buf.extend_from_slice(&0u32.to_le_bytes()); // reserved

    let mut data_offset: u64 = 0;
    let mut total_blocks = 0usize;

    for file in files {
        let source = &file.path;
        let data = read_and_decompress(source)?;

        let meta = fs::metadata(source)?;
        let file_size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        let num_blocks = (data.len() + BLOCK_SIZE - 1) / BLOCK_SIZE;

        // Relative path for portability
        let rel_path = source
            .file_name()
            .unwrap()
            .to_string_lossy();
        let path_bytes = rel_path.as_bytes();

        // Write file table entry
        index_buf.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
        index_buf.extend_from_slice(path_bytes);
        index_buf.extend_from_slice(&file_size.to_le_bytes());
        index_buf.extend_from_slice(&mtime.to_le_bytes());
        index_buf.extend_from_slice(&data_offset.to_le_bytes());
        index_buf.extend_from_slice(&(data.len() as u64).to_le_bytes());
        index_buf.extend_from_slice(&(num_blocks as u32).to_le_bytes());

        // Build and write blooms
        for block_idx in 0..num_blocks {
            let start = block_idx * BLOCK_SIZE;
            let end = std::cmp::min(start + BLOCK_SIZE, data.len());
            let bloom_filter = if json_mode {
                bloom::build_block_bloom_json(&data[start..end])
            } else {
                bloom::build_block_bloom(&data[start..end])
            };
            let bits = bloom_filter.as_bytes();
            index_buf.extend_from_slice(bits);
            if bits.len() < BLOOM_SIZE {
                index_buf.extend_from_slice(&vec![0u8; BLOOM_SIZE - bits.len()]);
            }
        }

        // Write decompressed data to consolidated .xgd
        data_file.write_all(&data)?;

        data_offset += data.len() as u64;
        total_blocks += num_blocks;
    }

    data_file.flush()?;
    fs::write(&index_path, &index_buf)?;

    let index_size = index_buf.len() as f64 / (1024.0 * 1024.0);
    let data_size = data_offset as f64 / (1024.0 * 1024.0);

    eprintln!(
        "[xgrep] consolidated index: {} files, {} blocks, {:.1}MB index, {:.1}MB data",
        files.len(),
        total_blocks,
        index_size,
        data_size,
    );

    Ok(())
}

/// Check if a consolidated index exists for a directory.
pub fn has_consolidated_index(dir: &Path) -> bool {
    let cache_dir = dir.join(".xgrep");
    cache_dir.join("index.xgi").exists() && cache_dir.join("index.xgd").exists()
}

/// Load consolidated index for a directory.
pub fn load_consolidated_index(dir: &Path) -> Result<ConsolidatedIndex> {
    let cache_dir = dir.join(".xgrep");
    let index_data = fs::read(cache_dir.join("index.xgi")).context("reading consolidated index")?;

    if index_data.len() < 24 || &index_data[0..8] != MAGIC_V2 {
        anyhow::bail!("invalid or old index format");
    }

    let file_count = u32::from_le_bytes(index_data[8..12].try_into().unwrap()) as usize;
    let _block_size = u32::from_le_bytes(index_data[12..16].try_into().unwrap()) as usize;
    let bloom_size = u32::from_le_bytes(index_data[16..20].try_into().unwrap()) as usize;

    let mut pos = 24; // after header
    let mut files = HashMap::new();

    for _ in 0..file_count {
        let path_len = u32::from_le_bytes(index_data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let path_str = String::from_utf8_lossy(&index_data[pos..pos + path_len]).to_string();
        pos += path_len;

        let source_size = u64::from_le_bytes(index_data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let source_mtime = u64::from_le_bytes(index_data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let data_offset = u64::from_le_bytes(index_data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let data_len = u64::from_le_bytes(index_data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let block_count = u32::from_le_bytes(index_data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        let mut blooms = Vec::with_capacity(block_count);
        for _ in 0..block_count {
            let bits = index_data[pos..pos + bloom_size].to_vec();
            blooms.push(bloom::BloomFilter::from_vec(bits));
            pos += bloom_size;
        }

        files.insert(
            path_str,
            IndexedFile {
                source_size,
                source_mtime,
                data_offset,
                data_len,
                blooms,
            },
        );
    }

    // Mmap the consolidated data file
    let data_file = File::open(cache_dir.join("index.xgd")).context("opening consolidated data")?;
    let data_mmap = unsafe { Mmap::map(&data_file).context("mmap consolidated data")? };

    Ok(ConsolidatedIndex {
        files,
        data_mmap,
    })
}

/// Search a file using the consolidated index.
pub fn consolidated_search(
    idx: &ConsolidatedIndex,
    file: &FileEntry,
    matcher: &Matcher,
    args: &Args,
    literals: &Option<Vec<u8>>,
    json_filters: &Option<Vec<JsonFilter>>,
) -> Option<(SearchResult, BlockSearchStats)> {
    let filename = file.path.file_name()?.to_string_lossy().to_string();
    let entry = idx.files.get(&filename)?;

    // Staleness check
    if let Ok(meta) = fs::metadata(&file.path) {
        if meta.len() != entry.source_size {
            return None;
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        if mtime != entry.source_mtime {
            return None;
        }
    }

    let data_start = entry.data_offset as usize;
    let data_end = data_start + entry.data_len as usize;
    if data_end > idx.data_mmap.len() {
        return None;
    }
    let data = &idx.data_mmap[data_start..data_end];
    let block_count = entry.blooms.len();
    let path_str = file.path.to_string_lossy().to_string();

    // Bloom prefilter
    let mut candidate_set = vec![false; block_count];
    let mut skipped = 0usize;

    if let Some(jf) = json_filters.as_ref() {
        // JSON mode: use field-value bloom pruning
        for (i, bloom_filter) in entry.blooms.iter().enumerate() {
            if bloom_filter.might_contain_json_query(jf) {
                candidate_set[i] = true;
            } else {
                skipped += 1;
            }
        }
    } else if let Some(lit) = literals.as_deref() {
        for (i, bloom_filter) in entry.blooms.iter().enumerate() {
            if bloom_filter.might_contain_query(lit, args.ignore_case) {
                candidate_set[i] = true;
            } else {
                skipped += 1;
            }
        }
    } else {
        candidate_set.fill(true);
    }

    let max_count = args.max_count.unwrap_or(usize::MAX);
    let needs_context = args.has_context();
    let count_only = (args.count || args.files_with_matches || args.quiet) && !needs_context;

    // Line-oriented iteration across the full data buffer.
    // A line's block is determined by where it STARTS (pos / BLOCK_SIZE).
    // Lines that span block boundaries are read fully — the newline search
    // extends across the entire data slice, not just the current block.

    if count_only {
        let mut count = 0usize;
        let mut pos = 0usize;

        while pos < data.len() {
            let line_end = memchr::memchr(b'\n', &data[pos..])
                .map(|i| pos + i)
                .unwrap_or(data.len());

            let block_idx = pos / BLOCK_SIZE;
            if block_idx < block_count && candidate_set[block_idx] {
                let line_end_trim = if line_end > pos && data[line_end - 1] == b'\r' {
                    line_end - 1
                } else {
                    line_end
                };

                let line = String::from_utf8_lossy(&data[pos..line_end_trim]);
                let matched = if let Some(jf) = json_filters.as_ref() {
                    crate::query::line_matches_filters(&line, jf)
                } else {
                    matcher.find_in_line(&line).is_some()
                };
                if matched {
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

        return Some((
            SearchResult {
                path: path_str,
                matches: dummy_matches,
                context_lines: Vec::new(),
            },
            BlockSearchStats {
                total_blocks: block_count,
                skipped_blocks: skipped,
                searched_blocks: block_count - skipped,
            },
        ));
    }

    // Full output path — line-oriented across full data
    let before = args.before_context;
    let after = args.after_context;
    let mut matches: Vec<Match> = Vec::new();
    let mut context_lines: Vec<(usize, String)> = Vec::new();
    let mut line_num = 0usize;
    let mut before_buf: Vec<(usize, String)> = Vec::with_capacity(before + 1);
    let mut after_remaining = 0usize;
    let mut pos = 0usize;

    while pos < data.len() && matches.len() < max_count {
        let line_end = memchr::memchr(b'\n', &data[pos..])
            .map(|i| pos + i)
            .unwrap_or(data.len());

        line_num += 1;

        let block_idx = pos / BLOCK_SIZE;
        let in_candidate = block_idx < block_count && candidate_set[block_idx];

        let line_end_trim = if line_end > pos && data[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };

        if in_candidate {
            let line_str = String::from_utf8_lossy(&data[pos..line_end_trim]);
            let match_result = if let Some(jf) = json_filters.as_ref() {
                if crate::query::line_matches_filters(&line_str, jf) {
                    Some((0, line_str.len()))
                } else {
                    None
                }
            } else {
                matcher.find_in_line(&line_str)
            };
            if let Some((start, end)) = match_result {
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

    Some((
        SearchResult {
            path: path_str,
            matches,
            context_lines,
        },
        BlockSearchStats {
            total_blocks: block_count,
            skipped_blocks: skipped,
            searched_blocks: block_count - skipped,
        },
    ))
}

/// Per-file index (v1 compat) — build
pub fn build_index(file: &FileEntry) -> Result<()> {
    let source = &file.path;
    let data = read_and_decompress(source)?;
    let num_blocks = (data.len() + BLOCK_SIZE - 1) / BLOCK_SIZE;

    let meta = fs::metadata(source)?;
    let file_size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    let cache_dir = cache_dir_for(source);
    fs::create_dir_all(&cache_dir)?;
    let stem = source.file_name().unwrap().to_string_lossy();

    fs::write(cache_dir.join(format!("{}.xgd", stem)), &data)?;

    let mut f = File::create(cache_dir.join(format!("{}.xgi", stem)))?;
    f.write_all(b"XGREP01\0")?;
    f.write_all(&file_size.to_le_bytes())?;
    f.write_all(&mtime.to_le_bytes())?;
    f.write_all(&(BLOCK_SIZE as u32).to_le_bytes())?;
    f.write_all(&(num_blocks as u32).to_le_bytes())?;
    f.write_all(&(BLOOM_SIZE as u32).to_le_bytes())?;
    f.write_all(&(data.len() as u32).to_le_bytes())?;

    for block_idx in 0..num_blocks {
        let start = block_idx * BLOCK_SIZE;
        let end = std::cmp::min(start + BLOCK_SIZE, data.len());
        let bloom_filter = bloom::build_block_bloom(&data[start..end]);
        let bits = bloom_filter.as_bytes();
        f.write_all(bits)?;
        if bits.len() < BLOOM_SIZE {
            f.write_all(&vec![0u8; BLOOM_SIZE - bits.len()])?;
        }
    }

    eprintln!(
        "[xgrep] indexed {}: {} blocks, {:.1}MB cached",
        stem,
        num_blocks,
        data.len() as f64 / (1024.0 * 1024.0)
    );

    Ok(())
}

/// Per-file index (v1) — check existence
pub fn has_cached_index(file: &FileEntry) -> bool {
    let source = &file.path;
    let cache_dir = cache_dir_for(source);
    let stem = source.file_name().unwrap().to_string_lossy();

    let index_path = cache_dir.join(format!("{}.xgi", stem));
    let data_path = cache_dir.join(format!("{}.xgd", stem));

    if !index_path.exists() || !data_path.exists() {
        return false;
    }

    match (fs::metadata(source), fs::read(&index_path)) {
        (Ok(meta), Ok(idx)) if idx.len() >= 40 => {
            if &idx[0..8] != b"XGREP01\0" {
                return false;
            }
            let cached_size = u64::from_le_bytes(idx[8..16].try_into().unwrap());
            let cached_mtime = u64::from_le_bytes(idx[16..24].try_into().unwrap());

            let file_size = meta.len();
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);

            cached_size == file_size && cached_mtime == mtime
        }
        _ => false,
    }
}

/// Per-file cached search (v1 compat)
pub fn cached_search(
    file: &FileEntry,
    matcher: &Matcher,
    args: &Args,
    literals: &Option<Vec<u8>>,
    json_filters: &Option<Vec<JsonFilter>>,
) -> Result<(SearchResult, BlockSearchStats)> {
    let source = &file.path;
    let cache_dir = cache_dir_for(source);
    let stem = source.file_name().unwrap().to_string_lossy();

    let index_data = fs::read(cache_dir.join(format!("{}.xgi", stem))).context("reading index")?;

    let block_size = u32::from_le_bytes(index_data[24..28].try_into().unwrap()) as usize;
    let block_count = u32::from_le_bytes(index_data[28..32].try_into().unwrap()) as usize;
    let bloom_size = u32::from_le_bytes(index_data[32..36].try_into().unwrap()) as usize;

    let bloom_start = 40;
    let mut blooms: Vec<bloom::BloomFilter> = Vec::with_capacity(block_count);
    for i in 0..block_count {
        let offset = bloom_start + i * bloom_size;
        let bits = index_data[offset..offset + bloom_size].to_vec();
        blooms.push(bloom::BloomFilter::from_vec(bits));
    }

    let data_file = File::open(cache_dir.join(format!("{}.xgd", stem))).context("opening cached data")?;
    let data = unsafe { Mmap::map(&data_file).context("mmap cached data")? };

    let path_str = file.path.to_string_lossy().to_string();

    let mut candidate_set = vec![false; block_count];
    let mut skipped = 0usize;

    if let Some(jf) = json_filters.as_ref() {
        for (i, bloom_filter) in blooms.iter().enumerate() {
            if bloom_filter.might_contain_json_query(jf) {
                candidate_set[i] = true;
            } else {
                skipped += 1;
            }
        }
    } else if let Some(lit) = literals.as_deref() {
        for (i, bloom_filter) in blooms.iter().enumerate() {
            if bloom_filter.might_contain_query(lit, args.ignore_case) {
                candidate_set[i] = true;
            } else {
                skipped += 1;
            }
        }
    } else {
        candidate_set.fill(true);
    }

    let max_count = args.max_count.unwrap_or(usize::MAX);
    let needs_context = args.has_context();
    let count_only = (args.count || args.files_with_matches || args.quiet) && !needs_context;

    if count_only {
        let mut count = 0usize;
        for block_idx in 0..block_count {
            if !candidate_set[block_idx] {
                continue;
            }
            let block_start = block_idx * block_size;
            let block_end = std::cmp::min(block_start + block_size, data.len());
            let block_data = &data[block_start..block_end];

            let mut pos = 0;
            while pos < block_data.len() {
                let line_end = memchr::memchr(b'\n', &block_data[pos..])
                    .map(|i| pos + i)
                    .unwrap_or(block_data.len());
                let line_end_trim = if line_end > pos && block_data[line_end - 1] == b'\r' {
                    line_end - 1
                } else {
                    line_end
                };
                let line = String::from_utf8_lossy(&block_data[pos..line_end_trim]);
                let matched = if let Some(jf) = json_filters.as_ref() {
                    crate::query::line_matches_filters(&line, jf)
                } else {
                    matcher.find_in_line(&line).is_some()
                };
                if matched {
                    count += 1;
                    if count >= max_count || args.quiet {
                        break;
                    }
                }
                pos = line_end + 1;
            }
            if count >= max_count || (args.quiet && count > 0) {
                break;
            }
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
                path: path_str,
                matches: dummy_matches,
                context_lines: Vec::new(),
            },
            BlockSearchStats {
                total_blocks: block_count,
                skipped_blocks: skipped,
                searched_blocks: block_count - skipped,
            },
        ));
    }

    // Full output — simplified for v1 compat
    anyhow::bail!("v1 cached full-output not reimplemented — use consolidated index")
}

fn read_and_decompress(path: &Path) -> Result<Vec<u8>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut data = Vec::new();
    if ext == "gz" || ext == "gzip" {
        let f = File::open(path)?;
        let mut decoder = flate2::read::GzDecoder::new(f);
        decoder.read_to_end(&mut data)?;
    } else {
        let mut f = File::open(path)?;
        f.read_to_end(&mut data)?;
    }
    Ok(data)
}

fn cache_dir_for(source: &Path) -> PathBuf {
    source.parent().unwrap_or(Path::new(".")).join(".xgrep")
}
