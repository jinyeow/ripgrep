use std::cmp;
use std::env;
use std::ffi::OsStr;
use std::io;
use std::ops;
use std::path::{Path, PathBuf};
use std::process;

use clap;
use env_logger;
use grep::{Grep, GrepBuilder};
use log;
use num_cpus;
use regex;
use term::Terminal;
#[cfg(not(windows))]
use term;
#[cfg(windows)]
use term::WinConsole;

use atty;
use app;
use ignore::overrides::{Override, OverrideBuilder};
use ignore::types::{FileTypeDef, Types, TypesBuilder};
use ignore;
use out::{Out, ColoredTerminal};
use printer::Printer;
#[cfg(windows)]
use terminal_win::WindowsBuffer;
use unescape::unescape;
use worker::{Worker, WorkerBuilder};

use {Result, version};

/// Args are transformed/normalized from ArgMatches.
#[derive(Debug)]
pub struct Args {
    paths: Vec<PathBuf>,
    after_context: usize,
    before_context: usize,
    color: bool,
    column: bool,
    context_separator: Vec<u8>,
    count: bool,
    files_with_matches: bool,
    eol: u8,
    files: bool,
    follow: bool,
    glob_overrides: Override,
    grep: Grep,
    heading: bool,
    hidden: bool,
    ignore_files: Vec<PathBuf>,
    invert_match: bool,
    line_number: bool,
    line_per_match: bool,
    max_count: Option<u64>,
    maxdepth: Option<usize>,
    mmap: bool,
    no_ignore: bool,
    no_ignore_parent: bool,
    no_ignore_vcs: bool,
    no_messages: bool,
    null: bool,
    quiet: bool,
    replace: Option<Vec<u8>>,
    text: bool,
    threads: usize,
    type_list: bool,
    types: Types,
    with_filename: bool,
}

impl Args {
    /// Parse the command line arguments for this process.
    ///
    /// If a CLI usage error occurred, then exit the process and print a usage
    /// or error message. Similarly, if the user requested the version of
    /// ripgrep, then print the version and exit.
    ///
    /// Also, initialize a global logger.
    pub fn parse() -> Result<Args> {
        let matches = app::app_short().get_matches();
        if matches.is_present("help-short") {
            let _ = ::app::app_short().print_help();
            let _ = println!("");
            process::exit(0);
        }
        if matches.is_present("help") {
            let _ = ::app::app_long().print_help();
            let _ = println!("");
            process::exit(0);
        }
        if matches.is_present("version") {
            println!("ripgrep {}", crate_version!());
            process::exit(0);
        }

        let mut logb = env_logger::LogBuilder::new();
        if matches.is_present("debug") {
            logb.filter(None, log::LogLevelFilter::Debug);
        } else {
            logb.filter(None, log::LogLevelFilter::Warn);
        }
        if let Err(err) = logb.init() {
            errored!("failed to initialize logger: {}", err);
        }
        ArgMatches(matches).to_args()
    }

    /// Returns true if ripgrep should print the files it will search and exit
    /// (but not do any actual searching).
    pub fn files(&self) -> bool {
        self.files
    }

    /// Create a new line based matcher. The matcher returned can be used
    /// across multiple threads simultaneously. This matcher only supports
    /// basic searching of regular expressions in a single buffer.
    ///
    /// The pattern and other flags are taken from the command line.
    pub fn grep(&self) -> Grep {
        self.grep.clone()
    }

    /// Whether ripgrep should be quiet or not.
    pub fn quiet(&self) -> bool {
        self.quiet
    }

    /// Create a new printer of individual search results that writes to the
    /// writer given.
    pub fn printer<W: Terminal + Send>(&self, wtr: W) -> Printer<W> {
        let mut p = Printer::new(wtr)
            .column(self.column)
            .context_separator(self.context_separator.clone())
            .eol(self.eol)
            .heading(self.heading)
            .line_per_match(self.line_per_match)
            .null(self.null)
            .with_filename(self.with_filename);
        if let Some(ref rep) = self.replace {
            p = p.replace(rep.clone());
        }
        p
    }

    /// Create a new printer of search results for an entire file that writes
    /// to the writer given.
    pub fn out(&self) -> Out {
        let mut out = Out::new(self.color);
        if let Some(filesep) = self.file_separator() {
            out = out.file_separator(filesep);
        }
        out
    }

    /// Retrieve the configured file separator.
    pub fn file_separator(&self) -> Option<Vec<u8>> {
        if self.heading && !self.count && !self.files_with_matches {
            Some(b"".to_vec())
        } else if self.before_context > 0 || self.after_context > 0 {
            Some(self.context_separator.clone())
        } else {
            None
        }
    }

    /// Returns true if the given arguments are known to never produce a match.
    pub fn never_match(&self) -> bool {
        self.max_count == Some(0)
    }

    /// Create a new buffer for use with searching.
    #[cfg(not(windows))]
    pub fn outbuf(&self) -> ColoredTerminal<term::TerminfoTerminal<Vec<u8>>> {
        ColoredTerminal::new(vec![], self.color)
    }

    /// Create a new buffer for use with searching.
    #[cfg(windows)]
    pub fn outbuf(&self) -> ColoredTerminal<WindowsBuffer> {
        ColoredTerminal::new_buffer(self.color)
    }

    /// Create a new buffer for use with searching.
    #[cfg(not(windows))]
    pub fn stdout(
        &self,
    ) -> ColoredTerminal<term::TerminfoTerminal<io::BufWriter<io::Stdout>>> {
        ColoredTerminal::new(io::BufWriter::new(io::stdout()), self.color)
    }

    /// Create a new buffer for use with searching.
    #[cfg(windows)]
    pub fn stdout(&self) -> ColoredTerminal<WinConsole<io::Stdout>> {
        ColoredTerminal::new_stdout(self.color)
    }

    /// Return the paths that should be searched.
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Returns true if there is exactly one file path given to search.
    pub fn is_one_path(&self) -> bool {
        self.paths.len() == 1
        && (self.paths[0] == Path::new("-") || self.paths[0].is_file())
    }

    /// Create a worker whose configuration is taken from the
    /// command line.
    pub fn worker(&self) -> Worker {
        WorkerBuilder::new(self.grep())
            .after_context(self.after_context)
            .before_context(self.before_context)
            .count(self.count)
            .files_with_matches(self.files_with_matches)
            .eol(self.eol)
            .line_number(self.line_number)
            .invert_match(self.invert_match)
            .max_count(self.max_count)
            .mmap(self.mmap)
            .quiet(self.quiet)
            .text(self.text)
            .build()
    }

    /// Returns the number of worker search threads that should be used.
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Returns a list of type definitions currently loaded.
    pub fn type_defs(&self) -> &[FileTypeDef] {
        self.types.definitions()
    }

    /// Returns true if ripgrep should print the type definitions currently
    /// loaded and then exit.
    pub fn type_list(&self) -> bool {
        self.type_list
    }

    /// Returns true if error messages should be suppressed.
    pub fn no_messages(&self) -> bool {
        self.no_messages
    }

    /// Create a new recursive directory iterator over the paths in argv.
    pub fn walker(&self) -> ignore::Walk {
        self.walker_builder().build()
    }

    /// Create a new parallel recursive directory iterator over the paths
    /// in argv.
    pub fn walker_parallel(&self) -> ignore::WalkParallel {
        self.walker_builder().build_parallel()
    }

    fn walker_builder(&self) -> ignore::WalkBuilder {
        let paths = self.paths();
        let mut wd = ignore::WalkBuilder::new(&paths[0]);
        for path in &paths[1..] {
            wd.add(path);
        }
        for path in &self.ignore_files {
            if let Some(err) = wd.add_ignore(path) {
                if !self.no_messages {
                    eprintln!("{}", err);
                }
            }
        }

        wd.follow_links(self.follow);
        wd.hidden(!self.hidden);
        wd.max_depth(self.maxdepth);
        wd.overrides(self.glob_overrides.clone());
        wd.types(self.types.clone());
        wd.git_global(!self.no_ignore && !self.no_ignore_vcs);
        wd.git_ignore(!self.no_ignore && !self.no_ignore_vcs);
        wd.git_exclude(!self.no_ignore && !self.no_ignore_vcs);
        wd.ignore(!self.no_ignore);
        wd.parents(!self.no_ignore_parent);
        wd.threads(self.threads());
        wd
    }
}

/// ArgMatches wraps clap::ArgMatches and provides semantic meaning to several
/// options/flags.
struct ArgMatches<'a>(clap::ArgMatches<'a>);

impl<'a> ops::Deref for ArgMatches<'a> {
    type Target = clap::ArgMatches<'a>;
    fn deref(&self) -> &clap::ArgMatches<'a> { &self.0 }
}

impl<'a> ArgMatches<'a> {
    /// Convert the result of parsing CLI arguments into ripgrep's
    /// configuration.
    fn to_args(&self) -> Result<Args> {
        let paths = self.paths();
        let mmap = try!(self.mmap(&paths));
        let with_filename = self.with_filename(&paths);
        let (before_context, after_context) = try!(self.contexts());
        let args = Args {
            paths: paths,
            after_context: after_context,
            before_context: before_context,
            color: self.color(),
            column: self.column(),
            context_separator: self.context_separator(),
            count: self.is_present("count"),
            files_with_matches: self.is_present("files-with-matches"),
            eol: b'\n',
            files: self.is_present("files"),
            follow: self.is_present("follow"),
            glob_overrides: try!(self.overrides()),
            grep: try!(self.grep()),
            heading: self.heading(),
            hidden: self.hidden(),
            ignore_files: self.ignore_files(),
            invert_match: self.is_present("invert-match"),
            line_number: self.line_number(),
            line_per_match: self.is_present("vimgrep"),
            max_count: try!(self.usize_of("max-count")).map(|max| max as u64),
            maxdepth: try!(self.usize_of("maxdepth")),
            mmap: mmap,
            no_ignore: self.no_ignore(),
            no_ignore_parent: self.no_ignore_parent(),
            no_ignore_vcs: self.no_ignore_vcs(),
            no_messages: self.is_present("no-messages"),
            null: self.is_present("null"),
            quiet: self.is_present("quiet"),
            replace: self.replace(),
            text: self.text(),
            threads: try!(self.threads()),
            type_list: self.is_present("type-list"),
            types: try!(self.types()),
            with_filename: with_filename,
        };
        if args.mmap {
            debug!("will try to use memory maps");
        }
        Ok(args)
    }

    /// Return all file paths that ripgrep should search.
    fn paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = match self.values_of_os("path") {
            None => vec![],
            Some(vals) => vals.map(|p| Path::new(p).to_path_buf()).collect(),
        };
        // If --files or --regexp is given, then the first path is always
        // in `pattern`.
        if self.is_present("files") || self.is_present("regexp") {
            if let Some(path) = self.value_of_os("pattern") {
                paths.insert(0, Path::new(path).to_path_buf());
            }
        }
        if paths.is_empty() {
            paths.push(self.default_path());
        }
        paths
    }

    /// Return the default path that ripgrep should search.
    fn default_path(&self) -> PathBuf {
        let search_cwd = atty::on_stdin()
            || self.is_present("files")
            || self.is_present("type-list")
            || !atty::stdin_is_readable();
        if search_cwd {
            Path::new("./").to_path_buf()
        } else {
            Path::new("-").to_path_buf()
        }
    }

    /// Return all of the ignore files given on the command line.
    fn ignore_files(&self) -> Vec<PathBuf> {
        match self.values_of_os("ignore-file") {
            None => return vec![],
            Some(vals) => vals.map(|p| Path::new(p).to_path_buf()).collect(),
        }
    }

    /// Return the pattern that should be used for searching.
    ///
    /// If multiple -e/--regexp flags are given, then they are all collapsed
    /// into one pattern.
    ///
    /// If any part of the pattern isn't valid UTF-8, then an error is
    /// returned.
    fn pattern(&self) -> Result<String> {
        match self.values_of_os("regexp") {
            None => {
                let pat = match self.value_of_os("pattern") {
                    None => return Ok("".to_string()),
                    Some(pat) => try!(pattern_to_str(pat)).to_string(),
                };
                Ok(self.word_pattern(self.literal_pattern(pat)))
            }
            Some(os_pats) => {
                let mut pats = vec![];
                for os_pat in os_pats {
                    let pat = try!(pattern_to_str(os_pat)).to_string();
                    pats.push(self.word_pattern(self.literal_pattern(pat)));
                }
                Ok(pats.join("|"))
            }
        }
    }

    /// Returns the given pattern as a literal pattern if the
    /// -F/--fixed-strings flag is set. Otherwise, the pattern is returned
    /// unchanged.
    fn literal_pattern(&self, pat: String) -> String {
        if self.is_present("fixed-strings") {
            regex::quote(&pat)
        } else {
            pat
        }
    }

    /// Returns the given pattern as a word pattern if the -w/--word-regexp
    /// flag is set. Otherwise, the pattern is returned unchanged.
    fn word_pattern(&self, pat: String) -> String {
        if self.is_present("word-regexp") {
            format!(r"\b{}\b", pat)
        } else {
            pat
        }
    }

    /// Returns true if and only if file names containing each match should
    /// be emitted.
    ///
    /// `paths` should be a slice of all top-level file paths that ripgrep
    /// will need to search.
    fn with_filename(&self, paths: &[PathBuf]) -> bool {
        if self.is_present("no-filename") {
            false
        } else {
            self.is_present("with-filename")
            || paths.len() > 1
            || paths.get(0).map_or(false, |p| p.is_dir())
        }
    }

    /// Returns true if and only if memory map searching should be tried.
    ///
    /// `paths` should be a slice of all top-level file paths that ripgrep
    /// will need to search.
    fn mmap(&self, paths: &[PathBuf]) -> Result<bool> {
        let (before, after) = try!(self.contexts());
        Ok(if before > 0 || after > 0 || self.is_present("no-mmap") {
            false
        } else if self.is_present("mmap") {
            true
        } else if cfg!(windows) {
            // On Windows, memory maps appear faster than read calls. Neat.
            true
        } else if cfg!(target_os = "macos") {
            // On Mac, memory maps appear to suck. Neat.
            false
        } else {
            // If we're only searching a few paths and all of them are
            // files, then memory maps are probably faster.
            paths.len() <= 10 && paths.iter().all(|p| p.is_file())
        })
    }

    /// Returns true if and only if line numbers should be shown.
    fn line_number(&self) -> bool {
        if self.is_present("no-line-number") || self.is_present("count") {
            false
        } else {
            self.is_present("line-number")
            || atty::on_stdout()
            || self.is_present("pretty")
            || self.is_present("vimgrep")
        }
    }

    /// Returns true if and only if column numbers should be shown.
    fn column(&self) -> bool {
        self.is_present("column") || self.is_present("vimgrep")
    }

    /// Returns true if and only if matches should be grouped with file name
    /// headings.
    fn heading(&self) -> bool {
        if self.is_present("no-heading") {
            false
        } else {
            self.is_present("heading")
            || atty::on_stdout()
            || self.is_present("pretty")
        }
    }

    /// Returns the replacement string as UTF-8 bytes if it exists.
    fn replace(&self) -> Option<Vec<u8>> {
        self.value_of_lossy("replace").map(|s| s.into_owned().into_bytes())
    }

    /// Returns the unescaped context separator in UTF-8 bytes.
    fn context_separator(&self) -> Vec<u8> {
        match self.value_of_lossy("context-separator") {
            None => b"--".to_vec(),
            Some(sep) => unescape(&sep),
        }
    }

    /// Returns the before and after contexts from the command line.
    ///
    /// If a context setting was absent, then `0` is returned.
    ///
    /// If there was a problem parsing the values from the user as an integer,
    /// then an error is returned.
    fn contexts(&self) -> Result<(usize, usize)> {
        let after = try!(self.usize_of("after-context")).unwrap_or(0);
        let before = try!(self.usize_of("before-context")).unwrap_or(0);
        let both = try!(self.usize_of("context")).unwrap_or(0);
        Ok(if both > 0 {
            (both, both)
        } else {
            (before, after)
        })
    }

    /// Returns true if and only if ripgrep should color its output.
    fn color(&self) -> bool {
        let preference = match self.0.value_of_lossy("color") {
            None => "auto".to_string(),
            Some(v) => v.into_owned(),
        };
        if preference == "always" {
            true
        } else if self.is_present("vimgrep") {
            false
        } else if preference == "auto" {
            atty::on_stdout() || self.is_present("pretty")
        } else {
            false
        }
    }

    /// Returns the approximate number of threads that ripgrep should use.
    fn threads(&self) -> Result<usize> {
        let threads = try!(self.usize_of("threads")).unwrap_or(0);
        Ok(if threads == 0 {
            cmp::min(12, num_cpus::get())
        } else {
            threads
        })
    }

    /// Builds a grep matcher from the command line flags.
    ///
    /// If there was a problem extracting the pattern from the command line
    /// flags, then an error is returned.
    fn grep(&self) -> Result<Grep> {
        let smart =
            self.is_present("smart-case")
            && !self.is_present("ignore-case")
            && !self.is_present("case-sensitive");
        let casei =
            self.is_present("ignore-case")
            && !self.is_present("case-sensitive");
        GrepBuilder::new(&try!(self.pattern()))
            .case_smart(smart)
            .case_insensitive(casei)
            .line_terminator(b'\n')
            .build()
            .map_err(From::from)
    }

    /// Builds the set of glob overrides from the command line flags.
    fn overrides(&self) -> Result<Override> {
        let mut ovr = OverrideBuilder::new(try!(env::current_dir()));
        for glob in self.values_of_lossy_vec("glob") {
            try!(ovr.add(&glob));
        }
        ovr.build().map_err(From::from)
    }

    /// Builds a file type matcher from the command line flags.
    fn types(&self) -> Result<Types> {
        let mut btypes = TypesBuilder::new();
        btypes.add_defaults();
        for ty in self.values_of_lossy_vec("type-clear") {
            btypes.clear(&ty);
        }
        for def in self.values_of_lossy_vec("type-add") {
            try!(btypes.add_def(&def));
        }
        for ty in self.values_of_lossy_vec("type") {
            btypes.select(&ty);
        }
        for ty in self.values_of_lossy_vec("type-not") {
            btypes.negate(&ty);
        }
        btypes.build().map_err(From::from)
    }

    /// Returns true if ignore files should be ignored.
    fn no_ignore(&self) -> bool {
        self.is_present("no-ignore")
        || self.occurrences_of("unrestricted") >= 1
    }

    /// Returns true if parent ignore files should be ignored.
    fn no_ignore_parent(&self) -> bool {
        self.is_present("no-ignore-parent") || self.no_ignore()
    }

    /// Returns true if VCS ignore files should be ignored.
    fn no_ignore_vcs(&self) -> bool {
        self.is_present("no-ignore-vcs") || self.no_ignore()
    }

    /// Returns true if and only if hidden files/directories should be
    /// searched.
    fn hidden(&self) -> bool {
        self.is_present("hidden") || self.occurrences_of("unrestricted") >= 2
    }

    /// Returns true if and only if all files should be treated as if they
    /// were text, even if ripgrep would detect it as a binary file.
    fn text(&self) -> bool {
        self.is_present("text") || self.occurrences_of("unrestricted") >= 3
    }

    /// Like values_of_lossy, but returns an empty vec if the flag is not
    /// present.
    fn values_of_lossy_vec(&self, name: &str) -> Vec<String> {
        self.values_of_lossy(name).unwrap_or(vec![])
    }

    /// Safely reads an arg value with the given name, and if it's present,
    /// tries to parse it as a usize value.
    fn usize_of(&self, name: &str) -> Result<Option<usize>> {
        match self.value_of_lossy(name) {
            None => Ok(None),
            Some(v) => v.parse().map(Some).map_err(From::from),
        }
    }
}

fn pattern_to_str(s: &OsStr) -> Result<&str> {
    match s.to_str() {
        Some(s) => Ok(s),
        None => Err(From::from(format!(
            "Argument '{}' is not valid UTF-8. \
             Use hex escape sequences to match arbitrary \
             bytes in a pattern (e.g., \\xFF).",
             s.to_string_lossy()))),
    }
}
