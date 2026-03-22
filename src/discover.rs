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

pub fn find_files(args: &Args) -> Result<Vec<FileEntry>> {
    let mut files = Vec::new();

    for input_path in &args.paths {
        let path = Path::new(input_path);

        if path.is_file() {
            if let Some(entry) = classify(path) {
                files.push(entry);
            }
        } else if path.is_dir() {
            for entry in WalkDir::new(path)
                .follow_links(true)
                .into_iter()
                .filter_map(|e| e.ok())
            {
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
