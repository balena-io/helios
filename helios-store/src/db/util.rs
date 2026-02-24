use std::fs;
use std::io;
use std::path::Path;

use super::glob::{self, GlobSet};

/// Options for configuring directory traversal with [`walk_dir`].
#[derive(Debug, Clone, Default)]
pub struct WalkDirOptions {
    /// The maximum depth to recurse. A value of `0` means no limit. Depth `1`
    /// corresponds to the direct descendents of the starting path, their
    /// descendents have depth `2`, and so on.
    pub max_depth: u8,
    /// The number of nested directory levels to skip. Depth `1` corresponds
    /// to the direct descendents of the starting path, their descendents have
    /// depth `2`, and so on. For consistency with `max_depth`, values `0` and `1`
    /// are equivalent and mean no limit.
    pub min_depth: u8,

    /// If `true`, the walker will follow symbolic links. Defaults to `false`.
    pub follow_symlinks: bool,
    /// If `true`, the walker will descend into directories that are on a
    /// different file system from the starting path. Defaults to `false`.
    pub cross_filesystems: bool,

    /// If `true`, hidden files and directories (those starting with a `.`)
    /// will be excluded from the results. Defaults to `false`.
    pub exclude_hidden: bool,
    /// If `true`, symlinks will be excluded from the results. Defaults to `false`.
    pub exclude_symlinks: bool,
    /// If `true`, files will be excluded from the results. Defaults to `false`.
    pub exclude_files: bool,
    /// If `true`, directories will be excluded from the results. Defaults to `false`.
    pub exclude_directories: bool,

    /// If `true`, block and character devices will be included. This is only
    /// applicable on Unix-like systems. Defaults to `false`.
    pub include_devices: bool,
    /// If `true`, FIFOs and sockets will be included. This is only applicable
    /// on Unix-like systems. Defaults to `false`.
    pub include_special: bool,

    /// A set of glob patterns to include. If non-empty, only paths matching
    /// at least one pattern are included. If empty, all paths are considered
    /// for inclusion (subject to exclusion rules). Directories not matched
    /// by this filter will not be traversed and their contents excluded.
    /// Patterns are matched against paths relative to the starting path.
    pub include: Vec<String>,
    /// A set of glob patterns to exclude. If a path matches any of these
    /// patterns, it is excluded. Exclusions take precedence over inclusions.
    /// Directories matched by this filter will not be traversed and their
    /// contents excluded as well. Patterns are matched against paths relative
    /// to the starting path.
    pub exclude: Vec<String>,
}

pub use walkdir::DirEntry;

/// Recursively walks a directory and yields its entries.
///
/// This function provides a configurable way to iterate over the contents of
/// a directory tree. It returns an iterator over `io::Result<DirEntry>`.
/// The starting `path` itself is not included in the results.
///
/// By default, the walker does not follow symlinks and stays on the same
/// filesystem as the starting path. Its behavior can be customized using
/// [`WalkDirOptions`].
///
/// # Example
///
/// ```ignore
/// use helios_store::db::{walk_dir, WalkDirOptions};
///
/// # fn example() -> std::io::Result<()> {
/// let opts = WalkDirOptions {
///     max_depth: 2,
///     exclude_hidden: true,
///     ..Default::default()
/// };
///
/// for entry in walk_dir("/my/directory", opts).unwrap() {
///     let entry = entry?;
///     println!("Found: {}", entry.path().display());
/// }
/// # Ok(())
/// # }
/// ```
pub fn walk_dir(
    path: impl AsRef<Path>,
    opts: WalkDirOptions,
) -> Result<impl Iterator<Item = io::Result<DirEntry>>, glob::GlobError> {
    // `max_open` allows descending into up to `max_depth` levels of
    // sub-directories without extra buffering.
    let (max_open, max_depth) = if opts.max_depth != 0 {
        (opts.max_depth as usize, opts.max_depth as usize)
    } else {
        (16, usize::MAX)
    };

    // Skip `path`.
    let min_depth = opts.min_depth.max(1) as usize;

    struct TraverseOpts {
        include: GlobSet,
        exclude: GlobSet,
        exclude_hidden: bool,
        exclude_symlinks: bool,
        exclude_files: bool,
        include_devices: bool,
        include_special: bool,
    }

    struct FilterOpts {
        include: GlobSet,
        exclude_directories: bool,
    }

    fn is_hidden(name: Option<&str>) -> bool {
        name.map(|s| s.starts_with(".")).unwrap_or(false)
    }

    #[cfg(unix)]
    fn is_device(ty: fs::FileType) -> bool {
        use std::os::unix::fs::FileTypeExt;
        ty.is_block_device() || ty.is_char_device()
    }

    #[cfg(unix)]
    fn is_special(ty: fs::FileType) -> bool {
        use std::os::unix::fs::FileTypeExt;
        ty.is_fifo() || ty.is_socket()
    }

    let traverse_opts = TraverseOpts {
        include: glob::incremental(opts.include.iter())?,
        exclude: glob::glob(opts.exclude)?,
        exclude_hidden: opts.exclude_hidden,
        exclude_symlinks: opts.exclude_symlinks,
        exclude_files: opts.exclude_files,
        include_devices: opts.include_devices,
        include_special: opts.include_special,
    };

    fn should_traverse(opts: &TraverseOpts, entry: &DirEntry, prefix: &Path) -> bool {
        let ty = entry.file_type();
        let excluded = (!opts.include_devices && is_device(ty))
            || (!opts.include_special && is_special(ty))
            || (opts.exclude_symlinks && ty.is_symlink())
            || (opts.exclude_files && ty.is_file())
            || (opts.exclude_hidden && is_hidden(entry.file_name().to_str()));
        !excluded && {
            let path = entry.path().strip_prefix(prefix).unwrap();
            (opts.include.is_empty() || opts.include.is_match(path))
                && (opts.exclude.is_empty() || !opts.exclude.is_match(path))
        }
    }

    let filter_opts = FilterOpts {
        include: glob::glob(opts.include)?,
        exclude_directories: opts.exclude_directories,
    };

    fn matches_filters(opts: &FilterOpts, entry: &DirEntry, prefix: &Path) -> bool {
        let excluded = opts.exclude_directories && entry.file_type().is_dir();
        !excluded && {
            opts.include.is_empty() || {
                let path = entry.path().strip_prefix(prefix).unwrap();
                opts.include.is_match(path)
            }
        }
    }

    let path_copy = path.as_ref().to_path_buf();

    let iter = walkdir::WalkDir::new(path.as_ref())
        .same_file_system(!opts.cross_filesystems)
        .follow_links(opts.follow_symlinks)
        .follow_root_links(opts.follow_symlinks)
        .max_open(max_open)
        .max_depth(max_depth)
        .min_depth(min_depth)
        .into_iter()
        .filter_entry(move |entry| should_traverse(&traverse_opts, entry, &path_copy))
        .filter(move |res| {
            res.as_ref()
                .map(|entry| matches_filters(&filter_opts, entry, path.as_ref()))
                .unwrap_or(true)
        })
        .map(|res| res.map_err(Into::into));

    Ok(iter)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    fn setup_test_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        // Directories
        fs::create_dir_all(root.join("src/module/submod")).unwrap();
        fs::create_dir_all(root.join("empty_dir")).unwrap();
        fs::create_dir_all(root.join(".hidden_dir")).unwrap();

        // Files
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("src/module/mod.rs"), "").unwrap();
        fs::write(root.join("src/module/submod/mod.rs"), "").unwrap();
        fs::write(root.join("README.md"), "").unwrap();
        fs::write(root.join("config.toml"), "").unwrap();
        fs::write(root.join(".hidden_file"), "").unwrap();
        fs::write(root.join(".hidden_dir/file.txt"), "").unwrap();

        // Symlinks
        #[cfg(unix)]
        {
            use std::os::unix::fs as unix_fs;
            unix_fs::symlink("src/main.rs", root.join("main_link.rs")).unwrap();
            unix_fs::symlink("src", root.join("src_link")).unwrap();
        }

        (dir, root)
    }

    fn get_paths(root: &Path, opts: WalkDirOptions) -> impl Iterator<Item = PathBuf> {
        walk_dir(root, opts)
            .unwrap()
            .map(move |r| r.unwrap().path().strip_prefix(root).unwrap().to_path_buf())
    }

    fn assert_paths_eq<A, B>(a: A, b: B)
    where
        A: IntoIterator,
        B: IntoIterator,
        A::Item: Into<PathBuf>,
        B::Item: Into<PathBuf>,
    {
        let mut a: Vec<_> = a.into_iter().map(|p| p.into()).collect();
        let mut b: Vec<_> = b.into_iter().map(|p| p.into()).collect();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b);
    }

    fn assert_matches<I>(root: &Path, opts: WalkDirOptions, expected: I)
    where
        I: IntoIterator,
        I::Item: Into<PathBuf>,
    {
        assert_paths_eq(get_paths(root, opts), expected);
    }

    #[test]
    fn test_walk_dir_default() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions::default(),
            [
                ".hidden_dir",
                ".hidden_dir/file.txt",
                ".hidden_file",
                "config.toml",
                "empty_dir",
                "README.md",
                "src",
                "src/lib.rs",
                "src/main.rs",
                "src/module",
                "src/module/mod.rs",
                "src/module/submod",
                "src/module/submod/mod.rs",
                #[cfg(unix)]
                "main_link.rs",
                #[cfg(unix)]
                "src_link",
            ],
        );
    }

    #[test]
    fn test_walk_dir_include() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions {
                include: vec!["src/module".to_string()],
                ..Default::default()
            },
            ["src/module"],
        );

        assert_matches(
            &root,
            WalkDirOptions {
                include: vec!["src/module/*".to_string()],
                ..Default::default()
            },
            ["src/module/mod.rs", "src/module/submod"],
        );

        assert_matches(
            &root,
            WalkDirOptions {
                include: vec!["src/**/*".to_string()],
                ..Default::default()
            },
            [
                "src/lib.rs",
                "src/main.rs",
                "src/module",
                "src/module/mod.rs",
                "src/module/submod",
                "src/module/submod/mod.rs",
            ],
        );

        assert_matches(
            &root,
            WalkDirOptions {
                include: vec!["**/*.rs".to_string()],
                ..Default::default()
            },
            [
                "src/lib.rs",
                "src/main.rs",
                "src/module/mod.rs",
                "src/module/submod/mod.rs",
                #[cfg(unix)]
                "main_link.rs",
            ],
        );
    }

    #[test]
    fn test_walk_dir_exclude() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions {
                exclude: vec!["**/*.rs".to_string()],
                ..Default::default()
            },
            [
                ".hidden_dir",
                ".hidden_dir/file.txt",
                ".hidden_file",
                "config.toml",
                "empty_dir",
                "README.md",
                "src",
                "src/module",
                "src/module/submod",
                #[cfg(unix)]
                "src_link",
            ],
        );

        assert_matches(
            &root,
            WalkDirOptions {
                exclude: vec!["**/*.*".to_string()],
                ..Default::default()
            },
            [
                "empty_dir",
                "src",
                "src/module",
                "src/module/submod",
                #[cfg(unix)]
                "src_link",
            ],
        );
    }

    #[test]
    fn test_walk_dir_exclude_hidden() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions {
                exclude_hidden: true,
                ..Default::default()
            },
            [
                "config.toml",
                "empty_dir",
                "README.md",
                "src",
                "src/lib.rs",
                "src/main.rs",
                "src/module",
                "src/module/mod.rs",
                "src/module/submod",
                "src/module/submod/mod.rs",
                #[cfg(unix)]
                "main_link.rs",
                #[cfg(unix)]
                "src_link",
            ],
        );
    }

    #[test]
    fn test_walk_dir_max_depth() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions {
                max_depth: 1,
                ..Default::default()
            },
            [
                ".hidden_dir",
                ".hidden_file",
                "config.toml",
                "empty_dir",
                "README.md",
                "src",
                #[cfg(unix)]
                "main_link.rs",
                #[cfg(unix)]
                "src_link",
            ],
        );
    }

    #[test]
    fn test_walk_dir_exclude_types() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions {
                exclude_files: true,
                exclude_symlinks: true,
                ..Default::default()
            },
            [
                ".hidden_dir",
                "empty_dir",
                "src",
                "src/module",
                "src/module/submod",
            ],
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_walk_dir_follow_symlinks() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions {
                follow_symlinks: true,
                ..Default::default()
            },
            [
                ".hidden_dir",
                ".hidden_dir/file.txt",
                ".hidden_file",
                "config.toml",
                "empty_dir",
                "main_link.rs",
                "README.md",
                "src",
                "src/lib.rs",
                "src/main.rs",
                "src/module",
                "src/module/mod.rs",
                "src/module/submod",
                "src/module/submod/mod.rs",
                "src_link",
                // These are from following symlinks
                "src_link/lib.rs",
                "src_link/main.rs",
                "src_link/module",
                "src_link/module/mod.rs",
                "src_link/module/submod",
                "src_link/module/submod/mod.rs",
            ],
        );
    }
}
