use crate::cli::Args;
use anyhow::Result;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub format: FileFormat,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FileFormat {
    PlainText,
    Gzip,
}

/// Directories pruned by default during recursive walks.
/// Covers VCS, language ecosystems, common build/cache/vendor patterns,
/// and xgrep's own cache (so `.xgrep/index.xgi` and `index.xgd` don't
/// appear in your own search results).
///
/// Disable the default list with `--no-ignore`. Add custom names with
/// repeated `--exclude <DIR>`.
const DEFAULT_EXCLUDE_DIRS: &[&str] = &[
    // xgrep's own cache (self-exclude — fixes index files appearing in results)
    ".xgrep",
    // VCS
    ".git",
    ".svn",
    ".hg",
    ".bzr",
    // Rust
    "target",
    // Node / JS / TS
    "node_modules",
    ".next",
    ".nuxt",
    // Python
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    // C / C++ / CMake (typical out-of-tree build dirs)
    "build",
    "cmake-build-debug",
    "cmake-build-release",
    // Generic build / dist outputs
    "dist",
    "out",
    // Vendored / third-party trees
    "3rdparty",
    "vendor",
    "third_party",
    // Editors / IDE
    ".idea",
    ".vscode",
];

/// Returns true if a directory name should be pruned from the walk.
/// Defaults are applied unless `use_defaults` is false (`--no-ignore`).
/// `custom_excludes` is the user-provided `--exclude` list (exact name match).
fn should_skip_dir(name: &str, custom_excludes: &[String], use_defaults: bool) -> bool {
    if use_defaults && DEFAULT_EXCLUDE_DIRS.contains(&name) {
        return true;
    }
    custom_excludes.iter().any(|e| e == name)
}

pub fn find_files(args: &Args) -> Result<Vec<FileEntry>> {
    let mut files = Vec::new();
    let use_defaults = !args.no_ignore;

    for input_path in &args.paths {
        let path = Path::new(input_path);

        if path.is_file() {
            if let Some(entry) = classify(path) {
                files.push(entry);
            }
        } else if path.is_dir() {
            // Use filter_entry to PRUNE excluded dirs before descending —
            // a regular filter would still walk into them and waste IO.
            let walker = WalkDir::new(path)
                .follow_links(true)
                .into_iter()
                .filter_entry(|e| {
                    // Never filter the user-supplied root or any non-dir entry.
                    if e.depth() == 0 || !e.file_type().is_dir() {
                        return true;
                    }
                    let name = e.file_name().to_string_lossy();
                    !should_skip_dir(&name, &args.exclude, use_defaults)
                });

            for entry in walker.filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() {
                    continue;
                }
                let p = entry.path();

                // Skip binary-looking files
                if is_likely_binary(p) {
                    continue;
                }

                // Apply --include glob filter
                if let Some(ref glob) = args.include {
                    if !matches_glob(p, glob) {
                        continue;
                    }
                }

                if let Some(fe) = classify(p) {
                    files.push(fe);
                }
            }
        }
    }

    Ok(files)
}

fn classify(path: &Path) -> Option<FileEntry> {
    let ext = path.extension()?.to_str()?;
    let format = match ext {
        "gz" | "gzip" => FileFormat::Gzip,
        "log" | "txt" | "csv" | "json" | "jsonl" | "ndjson" | "xml" | "yaml" | "yml" | "toml"
        | "md" | "rs" | "py" | "js" | "ts" | "go" | "c" | "cpp" | "h" | "java" | "rb"
        | "sh" | "bash" | "zsh" | "cfg" | "conf" | "ini" | "env" | "sql" | "html" | "css"
        | "lua" => FileFormat::PlainText,
        _ => {
            // No recognized extension — try to read as plain text if small enough
            // or if it has no extension at all
            FileFormat::PlainText
        }
    };

    Some(FileEntry {
        path: path.to_path_buf(),
        format,
    })
}

fn is_likely_binary(path: &Path) -> bool {
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e.to_lowercase(),
        None => return false,
    };

    matches!(
        ext.as_str(),
        "exe" | "dll" | "so" | "dylib" | "bin" | "o" | "a" | "lib"
            | "png" | "jpg" | "jpeg" | "gif" | "bmp" | "ico" | "webp"
            | "mp3" | "mp4" | "avi" | "mov" | "mkv" | "flac" | "wav"
            | "zip" | "tar" | "bz2" | "xz" | "zst" | "7z" | "rar"
            | "pdf" | "doc" | "docx" | "xls" | "xlsx" | "pptx"
            | "wasm" | "class" | "pyc" | "pyo"
    )
}

fn matches_glob(path: &Path, glob: &str) -> bool {
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };

    // Simple glob: *.ext or exact match
    if let Some(suffix) = glob.strip_prefix('*') {
        name.ends_with(suffix)
    } else {
        name == glob
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_excludes_well_known_dirs() {
        let custom: Vec<String> = vec![];
        // Defaults on
        assert!(should_skip_dir(".git", &custom, true));
        assert!(should_skip_dir("node_modules", &custom, true));
        assert!(should_skip_dir("target", &custom, true));
        assert!(should_skip_dir("__pycache__", &custom, true));
        assert!(should_skip_dir("3rdparty", &custom, true));
        // Self-exclude (the bug that motivated this)
        assert!(should_skip_dir(".xgrep", &custom, true));
        // Real source dirs not skipped
        assert!(!should_skip_dir("src", &custom, true));
        assert!(!should_skip_dir("tests", &custom, true));
        assert!(!should_skip_dir("xgrep", &custom, true)); // similar name, not skipped
    }

    #[test]
    fn test_no_ignore_disables_defaults() {
        let custom: Vec<String> = vec![];
        // --no-ignore should let the walker descend into target/, .git/, etc.
        assert!(!should_skip_dir(".git", &custom, false));
        assert!(!should_skip_dir("target", &custom, false));
        assert!(!should_skip_dir(".xgrep", &custom, false));
    }

    #[test]
    fn test_custom_excludes_apply_in_both_modes() {
        let custom = vec!["my_cache".to_string(), "generated".to_string()];
        // With defaults on: custom additions still match
        assert!(should_skip_dir("my_cache", &custom, true));
        assert!(should_skip_dir("generated", &custom, true));
        // With defaults off: only custom names match
        assert!(should_skip_dir("my_cache", &custom, false));
        assert!(!should_skip_dir(".git", &custom, false));
    }
}
