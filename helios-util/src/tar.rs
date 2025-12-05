use std::{io::Cursor, path::Path};

/// Filter all files that match the prefix in the archive stripping the prefix
///
/// Returns a new in-memory archive containing the files in the directory passed
/// as prefix
fn filter_with_prefix(bytes: &[u8], prefix: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    let mut filtered = Vec::new();
    {
        let mut reader = tar::Archive::new(Cursor::new(bytes));
        let mut writer = tar::Builder::new(Cursor::new(&mut filtered));

        let prefix = prefix.as_ref();
        let normalized_prefix = prefix.strip_prefix("/").unwrap_or(prefix);

        for entry in reader.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();

            if let Ok(stripped) = path.strip_prefix(normalized_prefix) {
                // Skip entries that result in empty paths (e.g., the directory itself)
                if stripped.as_os_str().is_empty() {
                    continue;
                }
                let mut header = entry.header().clone();
                writer.append_data(&mut header, stripped, &mut entry)?;
            }
        }

        writer.finish()?;
    }

    Ok(filtered)
}

pub fn unpack(bytes: &[u8], tgt_dir: impl AsRef<Path>) -> std::io::Result<()> {
    tar::Archive::new(Cursor::new(bytes)).unpack(tgt_dir)
}

/// Unpack only the specified directory from the given tar archive
///
/// This strips the prefix from the archive files, meaning the files will be extracted
/// to the base target dir.
///
/// ```rust,ignore
/// // tar archive bytes including files under `/foo` and `/bar`
/// let bytes = [/*..*/];
///
/// // this will create /tgt/foo and /tgt/bar
/// unpack(bytes, "/tgt")?;
///
/// // this will extract only /foo, copying the files to /tgt
/// unpack_from(bytes, "/foo", "/tgt")?;
/// ```
pub fn unpack_from(
    bytes: &[u8],
    src_dir: impl AsRef<Path>,
    tgt_dir: impl AsRef<Path>,
) -> std::io::Result<()> {
    let bytes = filter_with_prefix(bytes, src_dir)?;
    unpack(&bytes, tgt_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn create_test_archive(entries: Vec<(&str, &[u8])>) -> Vec<u8> {
        let mut archive = Vec::new();
        {
            let mut builder = tar::Builder::new(Cursor::new(&mut archive));

            for (path, content) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, path, content).unwrap();
            }

            builder.finish().unwrap();
        }
        archive
    }

    fn read_archive_paths(archive: &[u8]) -> Vec<String> {
        let mut paths = Vec::new();
        let mut reader = tar::Archive::new(Cursor::new(archive));

        for entry in reader.entries().unwrap() {
            let entry = entry.unwrap();
            paths.push(entry.path().unwrap().to_string_lossy().into_owned());
        }

        paths
    }

    #[test]
    fn test_filter_with_prefix_basic() {
        let archive = create_test_archive(vec![
            ("foo/bar/file1.txt", b"content1"),
            ("foo/bar/file2.txt", b"content2"),
            ("foo/baz/file3.txt", b"content3"),
        ]);

        let result = filter_with_prefix(&archive, "foo/bar").unwrap();
        let paths = read_archive_paths(&result);

        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"file1.txt".to_string()));
        assert!(paths.contains(&"file2.txt".to_string()));
    }

    #[test]
    fn test_filter_with_prefix_normalized() {
        let archive = create_test_archive(vec![
            ("foo/bar/file1.txt", b"content1"),
            ("foo/bar/file2.txt", b"content2"),
            ("foo/baz/file3.txt", b"content3"),
        ]);

        let result = filter_with_prefix(&archive, "/foo/bar").unwrap();
        let paths = read_archive_paths(&result);

        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"file1.txt".to_string()));
        assert!(paths.contains(&"file2.txt".to_string()));
    }

    #[test]
    fn test_filter_with_prefix_no_matches() {
        let archive = create_test_archive(vec![
            ("foo/bar/file1.txt", b"content1"),
            ("foo/bar/file2.txt", b"content2"),
        ]);

        let result = filter_with_prefix(&archive, "baz").unwrap();
        let paths = read_archive_paths(&result);

        assert_eq!(paths.len(), 0);
    }

    #[test]
    fn test_filter_with_prefix_filters_non_matching() {
        let archive = create_test_archive(vec![
            ("foo/bar/file1.txt", b"content1"),
            ("foo/baz/file2.txt", b"content2"),
            ("other/file3.txt", b"content3"),
        ]);

        let result = filter_with_prefix(&archive, "foo/bar").unwrap();
        let paths = read_archive_paths(&result);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], "file1.txt");
    }

    #[test]
    fn test_filter_with_prefix_nested_paths() {
        let archive = create_test_archive(vec![
            ("app/src/main.rs", b"fn main() {}"),
            ("app/src/lib.rs", b"pub fn lib() {}"),
            ("app/tests/test.rs", b"#[test]"),
        ]);

        let result = filter_with_prefix(&archive, "app/src").unwrap();
        let paths = read_archive_paths(&result);

        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"main.rs".to_string()));
        assert!(paths.contains(&"lib.rs".to_string()));
    }

    #[test]
    fn test_filter_with_prefix_preserves_content() {
        let archive = create_test_archive(vec![("dir/file.txt", b"test content")]);

        let result = filter_with_prefix(&archive, "dir").unwrap();

        let mut reader = tar::Archive::new(Cursor::new(result));
        let mut entries = reader.entries().unwrap();
        let mut entry = entries.next().unwrap().unwrap();

        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();

        assert_eq!(content, b"test content");
        assert_eq!(entry.path().unwrap().to_str().unwrap(), "file.txt");
    }

    #[test]
    fn test_filter_with_prefix_skips_empty_paths() {
        // Create archive with directory entry that matches prefix exactly
        let mut archive = Vec::new();
        {
            let mut builder = tar::Builder::new(Cursor::new(&mut archive));

            // Add directory entry for "app/"
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "app/", &mut &[][..])
                .unwrap();

            // Add file entry under the directory
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "app/file.txt", &mut &b"test"[..])
                .unwrap();

            builder.finish().unwrap();
        }

        // Strip "app" prefix - should skip the "app/" directory entry but keep the file
        let result = filter_with_prefix(&archive, "app").unwrap();
        let paths = read_archive_paths(&result);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], "file.txt");
    }
}
