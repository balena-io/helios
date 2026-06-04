use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Parse the `btime` (boot time, seconds since the Unix epoch) line out of the
/// contents of `/proc/stat`.
pub fn parse_btime(stat: &str) -> Option<SystemTime> {
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|secs| secs.trim().parse::<u64>().ok())
        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs))
}

/// Read the host boot time from `/proc/stat`.
pub fn boot_time() -> io::Result<SystemTime> {
    let stat = std::fs::read_to_string("/proc/stat")?;
    parse_btime(&stat).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "no btime in /proc/stat")
    })
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
}
