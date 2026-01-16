//! Utilities for working with glob patterns.
//!
//! This module provides a convenient way to compile glob patterns into a [`GlobSet`]
//! for efficient matching against file paths. It is a lightweight wrapper around
//! the [`globset`] crate, configured with a specific set of options for
//! case-insensitive matching and standard glob syntax.

use globset::{Glob, GlobBuilder};

pub use globset::{Error as GlobError, GlobSet};

/// Compiles a slice of glob patterns into a [`GlobSet`].
///
/// This function provides a convenient way to build a `GlobSet` for matching
/// file paths. It is configured for case-insensitive matching and treats path
/// separators literally.
///
/// # Example
///
/// ```rust,ignore
/// use helios_store::util::glob;
///
/// let globs = glob::glob(["**/*.rs", "*.md"]).unwrap();
/// assert!(globs.is_match("src/main.rs"));
/// assert!(globs.is_match("README.md"));
/// ```
///
/// # Syntax
///
/// Standard Unix-style glob syntax is supported:
///
/// - `?` matches any single character, except a path separator.
///
/// - `*` matches zero or more characters, except a path separator.
///
/// - `**` recursively matches directories but are only legal in three situations:
///
///   - First, if the glob starts with `**/`, then it matches all directories.
///     For example, `**/foo` matches `foo` and `bar/foo` but not `foo/bar`.
///   - Secondly, if the glob ends with `/**`, then it matches all sub-entries.
///     For example, `foo/**` matches `foo/a` and `foo/a/b`, but not `foo`.
///   - Thirdly, if the glob contains `/**/` anywhere within the pattern, then
///     it matches zero or more directories.
///
///   The glob `**` is allowed and means “match everything”.
///
/// - `{a,b}` matches `a` or `b` where `a` and `b` are arbitrary glob patterns.
///   Nesting `{...}` is supported.
///
/// - `[ab]` matches `a` or `b` where `a` and `b` are characters. Use `[!ab]` to
///   match any character except for `a` and `b`.
///
/// - Metacharacters such as `*` and `?` can be escaped with character class notation.
///   Eg., `[*]` matches `*`.
///
/// - A backslash (`\`) will escape all meta characters in a glob. If it precedes
///   a non-meta character, then the slash is ignored. A `\\` will match a literal `\\`.
pub fn glob<I>(patterns: I) -> Result<GlobSet, GlobError>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut set = GlobSet::builder();
    for pattern in patterns {
        set.add(build(pattern.as_ref())?);
    }
    set.build()
}

/// This is currently used by fs::walk_dir() to incrementally apply a glob
/// while directories are being read.
///
/// The glob set returned by this function will match sub-paths, eg. the glob
/// `src/*/README.md` will match `src`, `src/foo`, `src/bar`, in addition to
/// `src/foo/README.md` and `src/bar/README.md` that the glob as returned by
/// [`glob()`] would match.
///
/// This is marked `pub(crate)` because its behavior is somewhat counter-
/// intuitive unless it is exactly what you need, like in walk_dir. Also, the
/// name `incremental` is rubbish.
pub(crate) fn incremental<I>(patterns: I) -> Result<GlobSet, GlobError>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut set = GlobSet::builder();

    for pattern in patterns {
        let parts: Vec<_> = pattern.as_ref().split("/").collect();

        for (index, _) in parts.iter().enumerate() {
            let str = parts
                .iter()
                .take(index + 1)
                .copied()
                .collect::<Vec<_>>()
                .join("/");

            set.add(build(&str)?);
        }
    }

    set.build()
}

fn build(pattern: &str) -> Result<Glob, GlobError> {
    GlobBuilder::new(pattern)
        .allow_unclosed_class(false)
        .backslash_escape(true)
        .case_insensitive(true)
        .empty_alternates(true)
        .literal_separator(true)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_pattern() {
        // unclosed alternate
        assert!(glob(["{a,b"]).is_err());
        // unclosed class
        assert!(glob(["[ab"]).is_err());
    }

    #[test]
    fn test_literal_separator() {
        // `?` should not match `/`
        let globs = glob(["a?b"]).unwrap();
        assert!(!globs.is_match("a/b"));

        // `*` should not match `/`
        let globs = glob(["a*b"]).unwrap();
        assert!(!globs.is_match("a/x/b"));
        assert!(globs.is_match("axyb"));

        // `**` not adjacent to a separator is treated as `*` + `*`.
        let globs = glob(["foo**"]).unwrap();
        assert!(globs.is_match("foo"));
        assert!(globs.is_match("foobar"));
        assert!(!globs.is_match("foo/bar"));

        let globs = glob(["**/foo/**"]).unwrap();
        assert!(globs.is_match("foo/bar"));
        assert!(globs.is_match("a/foo/b"));
        assert!(globs.is_match("a/b/foo/c/d"));
    }

    #[test]
    fn test_question_mark() {
        let globs = glob(["f?o.txt"]).unwrap();
        assert!(globs.is_match("foo.txt"));
        assert!(globs.is_match("fOo.tXt")); // case-insensitive
        assert!(!globs.is_match("fo.txt"));
        assert!(!globs.is_match("fooo.txt"));
        assert!(!globs.is_match("f/o.txt")); // should not match separator
    }

    #[test]
    fn test_star() {
        let globs = glob(["f*.txt"]).unwrap();
        assert!(globs.is_match("f.txt"));
        assert!(globs.is_match("fo.txt"));
        assert!(globs.is_match("foo.txt"));
        assert!(globs.is_match("foobar.txt"));
        assert!(!globs.is_match("foo.tx"));
        assert!(!globs.is_match("bar/foo.txt"));
        assert!(!globs.is_match("f/oo.txt")); // should not match separator
    }

    #[test]
    fn test_double_star_prefix() {
        let globs = glob(["**/foo"]).unwrap();
        assert!(globs.is_match("foo"));
        assert!(globs.is_match("bar/foo"));
        assert!(globs.is_match("baz/bar/foo"));
        assert!(!globs.is_match("foo/bar"));
    }

    #[test]
    fn test_double_star_suffix() {
        let globs = glob(["foo/**"]).unwrap();
        assert!(globs.is_match("foo/bar"));
        assert!(globs.is_match("foo/bar/baz"));
        assert!(!globs.is_match("foo")); // does not match foo itself
        assert!(!globs.is_match("bar/foo"));
    }

    #[test]
    fn test_double_star_infix() {
        let globs = glob(["foo/**/bar"]).unwrap();
        assert!(globs.is_match("foo/bar"));
        assert!(globs.is_match("foo/baz/bar"));
        assert!(globs.is_match("foo/baz/qux/bar"));
        assert!(!globs.is_match("foo/bar/baz"));
    }

    #[test]
    fn test_double_star_only() {
        let globs = glob(["**"]).unwrap();
        assert!(globs.is_match("foo"));
        assert!(globs.is_match("foo/bar"));
        assert!(globs.is_match("foo/bar/baz.txt"));
    }

    #[test]
    fn test_alternates() {
        let globs = glob(["{foo,bar}/*.txt"]).unwrap();
        assert!(globs.is_match("foo/a.txt"));
        assert!(globs.is_match("bar/b.txt"));
        assert!(!globs.is_match("baz/c.txt"));
        assert!(!globs.is_match("foo/bar/a.txt"));

        // Nested alternates are supported.
        let globs = glob(["{foo,{bar,baz}}"]).unwrap();
        assert!(globs.is_match("foo"));
        assert!(globs.is_match("bar"));
        assert!(globs.is_match("baz"));
        assert!(!globs.is_match("qux"));

        // Empty alternates are allowed.
        let globs = glob(["{a,b,}", "x{y,z,}", "{}w"]).unwrap();
        assert!(globs.is_match("a"));
        assert!(globs.is_match("b"));
        assert!(globs.is_match("xy"));
        assert!(globs.is_match("xz"));
        assert!(globs.is_match("x"));
        assert!(globs.is_match("w"));
    }

    #[test]
    fn test_character_class() {
        let globs = glob(["foo[ab].txt"]).unwrap();
        assert!(globs.is_match("fooa.txt"));
        assert!(globs.is_match("foob.txt"));
        assert!(!globs.is_match("fooc.txt"));
    }

    #[test]
    fn test_negated_character_class() {
        let globs = glob(["foo[!ab].txt"]).unwrap();
        assert!(!globs.is_match("fooa.txt"));
        assert!(!globs.is_match("foob.txt"));
        assert!(globs.is_match("fooc.txt"));
    }

    #[test]
    fn test_escape_with_brackets() {
        let globs = glob(["foo[*].txt"]).unwrap();
        assert!(globs.is_match("foo*.txt"));
        assert!(!globs.is_match("fooa.txt"));
    }

    #[test]
    fn test_escape_with_backslash() {
        let globs = glob([r"foo\*.txt", r"bar\?.txt", r"baz\\qux"]).unwrap();
        assert!(globs.is_match("foo*.txt"));
        assert!(!globs.is_match("fooa.txt"));
        assert!(globs.is_match("bar?.txt"));
        assert!(!globs.is_match("bara.txt"));
        assert!(globs.is_match(r"baz\qux"));
    }

    #[test]
    fn test_case_insensitive() {
        let globs = glob(["Foo/Bar.Txt"]).unwrap();
        assert!(globs.is_match("foo/bar.txt"));
        assert!(globs.is_match("FOO/BAR.TXT"));
        assert!(globs.is_match("fOo/bAr.tXt"));
    }

    #[test]
    fn test_multiple_patterns() {
        let globs = glob(["*.log", "foo/bar.txt"]).unwrap();
        assert!(globs.is_match("system.log"));
        assert!(globs.is_match("foo/bar.txt"));
        assert!(!globs.is_match("system.txt"));
        assert!(!globs.is_match("foo/baz.txt"));
    }
}
