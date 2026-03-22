use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = "xgrep", version, about = "Fast grep for compressed and archived data")]
pub struct Args {
    /// Search pattern (not required with --build-index)
    pub pattern: String,

    /// Files or directories to search
    pub paths: Vec<String>,

    /// Treat pattern as fixed string (no regex)
    #[arg(short = 'F', long)]
    pub fixed_strings: bool,

    /// Use extended regex
    #[arg(short = 'E', long)]
    pub extended_regexp: bool,

    /// Case-insensitive matching
    #[arg(short = 'i', long)]
    pub ignore_case: bool,

    /// Print line numbers
    #[arg(short = 'n', long)]
    pub line_number: bool,

    /// Print only filenames of matching files
    #[arg(short = 'l', long)]
    pub files_with_matches: bool,

    /// Print only count of matching lines
    #[arg(short = 'c', long)]
    pub count: bool,

    /// Quiet mode — exit 0 on first match
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Print N lines after match
    #[arg(short = 'A', long, default_value = "0")]
    pub after_context: usize,

    /// Print N lines before match
    #[arg(short = 'B', long, default_value = "0")]
    pub before_context: usize,

    /// Print N lines before and after match
    #[arg(short = 'C', long)]
    pub context: Option<usize>,

    /// Recurse into directories
    #[arg(short = 'r', long)]
    pub recursive: bool,

    /// Include only files matching glob
    #[arg(long)]
    pub include: Option<String>,

    /// Print only matched parts
    #[arg(short = 'o', long)]
    pub only_matching: bool,

    /// Suppress filename prefix
    #[arg(long = "no-filename")]
    pub no_filename: bool,

    /// Always print filename prefix
    #[arg(short = 'H', long = "with-filename")]
    pub with_filename: bool,

    /// Stop after N matches per file
    #[arg(short = 'm', long)]
    pub max_count: Option<usize>,

    /// JSON field filter mode (NDJSON/JSONL).
    /// Pattern is parsed as field=value clauses instead of text/regex.
    /// Example: -j 'user_id=12345 status=500'
    #[arg(short = 'j', long = "json")]
    pub json: bool,

    /// Disable color output
    #[arg(long)]
    pub no_color: bool,

    /// Show block-skip statistics on stderr
    #[arg(long)]
    pub stats: bool,

    /// Build index cache for faster repeated queries
    #[arg(long)]
    pub build_index: bool,
}

impl Args {
    pub fn parse_args() -> anyhow::Result<Self> {
        let mut args = Self::parse();

        // --build-index: pattern is actually a path, no search pattern needed
        if args.build_index {
            // Shift: pattern becomes first path
            args.paths.insert(0, args.pattern.clone());
            args.pattern = String::new();
        }

        // Default to current directory if no paths specified
        if args.paths.is_empty() {
            args.paths.push(".".to_string());
        }

        // -C sets both -A and -B
        if let Some(ctx) = args.context {
            args.after_context = ctx;
            args.before_context = ctx;
        }

        // Auto-enable recursive for directory arguments
        for path in &args.paths {
            let p = std::path::Path::new(path);
            if p.is_dir() {
                args.recursive = true;
                break;
            }
        }

        Ok(args)
    }

    pub fn show_filename(&self) -> bool {
        if self.no_filename {
            return false;
        }
        if self.with_filename {
            return true;
        }
        // Show filename when searching multiple files
        self.paths.len() > 1 || self.recursive
    }

    pub fn has_context(&self) -> bool {
        self.after_context > 0 || self.before_context > 0
    }
}
