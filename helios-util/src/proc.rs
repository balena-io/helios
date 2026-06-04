use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Parse the `btime` (boot time, seconds since the Unix epoch) line out of the
/// contents of `/proc/stat`.
fn parse_btime(stat: &str) -> Option<SystemTime> {
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|secs| secs.trim().parse::<u64>().ok())
        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs))
}

/// Read the host boot time from `/proc/stat`.
pub fn boot_time() -> io::Result<SystemTime> {
    let stat = std::fs::read_to_string("/proc/stat")?;
    parse_btime(&stat)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no btime in /proc/stat"))
}

/// Parse the `balena_kernel_abi` token out of the contents of `/proc/cmdline`.
fn parse_kernel_abi(cmdline: &str) -> Option<String> {
    cmdline
        .split_ascii_whitespace()
        .find_map(|param| param.strip_prefix("balena_kernel_abi="))
        .filter(|abi| !abi.is_empty())
        .map(str::to_string)
}

/// Read the ABI id of the running kernel from `/proc/cmdline`.
pub fn kernel_abi() -> io::Result<Option<String>> {
    let cmdline = std::fs::read_to_string("/proc/cmdline")?;
    Ok(parse_kernel_abi(&cmdline))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_btime_line_among_others() {
        let stat = "cpu  1 2 3\nbtime 1700000000\nprocesses 99\n";
        assert_eq!(
            parse_btime(stat),
            Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        );
    }

    #[test]
    fn returns_none_without_btime() {
        assert_eq!(parse_btime("cpu 1 2 3\n"), None);
    }

    #[test]
    fn parses_the_kernel_abi_token_among_other_params() {
        let cmdline = "coherent_pool=1M root=UUID=abc balena_kernel_abi=a2a156ea70ff rootwait\n";
        assert_eq!(parse_kernel_abi(cmdline), Some("a2a156ea70ff".to_string()));
    }

    #[test]
    fn reports_no_token_on_a_stock_kernel_boot() {
        assert_eq!(parse_kernel_abi("root=UUID=abc rootwait\n"), None);
    }

    #[test]
    fn reports_no_token_for_an_empty_value() {
        // `kexec` appends the parameter only when it resolved an override, so an
        // empty value is malformed rather than meaningful. Treating it as absent
        // keeps it from matching an overlay whose label is somehow also empty.
        assert_eq!(
            parse_kernel_abi("root=UUID=abc balena_kernel_abi= rw\n"),
            None
        );
    }

    #[test]
    fn does_not_match_a_parameter_that_merely_ends_with_the_token_name() {
        assert_eq!(
            parse_kernel_abi("root=UUID=abc xbalena_kernel_abi=nope\n"),
            None
        );
    }
}
