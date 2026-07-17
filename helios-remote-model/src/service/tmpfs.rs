//! Service tmpfs mounts as defined by the Compose spec.
//!
//! The `tmpfs` field on a service mounts a temporary file system inside the
//! container. It can be a single value or a list of entries in the form
//! `<path>` or `<path>:<options>`, where `<options>` is a comma-separated
//! list of `mode`, `uid` and `gid` settings (e.g.
//! `mode=755,uid=1009,gid=1009`). Any other option is rejected.

use std::str::FromStr;

pub type Tmpfs = (String, TmpfsOptions);

/// Recognized tmpfs mount options.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TmpfsOptions {
    /// File system permissions, as octal digits (e.g. `755`)
    pub mode: Option<String>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl FromStr for TmpfsOptions {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let mut opts = TmpfsOptions::default();
        for token in raw.split(',').filter(|t| !t.is_empty()) {
            let (key, value) = token
                .split_once('=')
                .ok_or_else(|| format!("option `{token}` must be in `key=value` form"))?;
            match key {
                "mode" => {
                    if value.is_empty() || !value.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
                        return Err(format!("`mode` must be octal digits, got `{value}`"));
                    }
                    opts.mode = Some(value.to_string());
                }
                "uid" => {
                    opts.uid = Some(
                        value
                            .parse()
                            .map_err(|_| format!("`uid` must be a number, got `{value}`"))?,
                    );
                }
                "gid" => {
                    opts.gid = Some(
                        value
                            .parse()
                            .map_err(|_| format!("`gid` must be a number, got `{value}`"))?,
                    );
                }
                other => return Err(format!("unsupported option `{other}`")),
            }
        }
        Ok(opts)
    }
}

/// Parse a single `<path>` / `<path>:<options>` compose entry into a
/// path -> options pair
pub(super) fn try_from_entry(spec: &str) -> Result<Tmpfs, String> {
    let (path, options) = spec.split_once(':').unwrap_or((spec, ""));
    if path.is_empty() {
        return Err("path cannot be empty".to_string());
    }
    Ok((path.to_string(), options.parse()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_only() {
        assert_eq!(
            try_from_entry("/run").unwrap(),
            ("/run".to_string(), TmpfsOptions::default())
        );
    }

    #[test]
    fn path_with_options() {
        assert_eq!(
            try_from_entry("/data:mode=755,uid=1009,gid=1009").unwrap(),
            (
                "/data".to_string(),
                TmpfsOptions {
                    mode: Some("755".to_string()),
                    uid: Some(1009),
                    gid: Some(1009),
                }
            )
        );
    }

    #[test]
    fn rejects_empty_path() {
        let err = try_from_entry(":ro").unwrap_err();
        assert!(err.contains("path cannot be empty"), "{err}");
    }

    #[test]
    fn rejects_non_octal_mode() {
        let err = try_from_entry("/run:mode=999").unwrap_err();
        assert!(err.contains("octal"), "{err}");
    }

    #[test]
    fn rejects_non_numeric_uid() {
        let err = try_from_entry("/run:uid=abc").unwrap_err();
        assert!(err.contains("`uid`"), "{err}");
    }

    #[test]
    fn rejects_unsupported_option() {
        let err = try_from_entry("/run:size=100m").unwrap_err();
        assert!(err.contains("unsupported option"), "{err}");
    }

    #[test]
    fn rejects_flag_without_value() {
        let err = try_from_entry("/run:noexec").unwrap_err();
        assert!(err.contains("key=value"), "{err}");
    }
}
