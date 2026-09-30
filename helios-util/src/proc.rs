use std::io;

/// Parse the boot id out of the contents of `/proc/sys/kernel/random/boot_id`.
///
/// An empty file yields no id: it must never compare equal to an absent label,
/// which would read an overlay staged in this boot as predating it.
fn parse_boot_id(contents: &str) -> Option<&str> {
    let id = contents.trim();
    (!id.is_empty()).then_some(id)
}

/// Read the id of the running boot from `/proc/sys/kernel/random/boot_id`.
///
/// The kernel generates a fresh random UUID for every boot.
pub fn boot_id() -> io::Result<String> {
    let contents = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    parse_boot_id(&contents).map(str::to_string).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "empty /proc/sys/kernel/random/boot_id",
        )
    })
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
    fn parses_the_boot_id_without_its_trailing_newline() {
        assert_eq!(
            parse_boot_id("3f2b6c1a-8e4d-4a19-9c77-2f5a1b0d6e83\n"),
            Some("3f2b6c1a-8e4d-4a19-9c77-2f5a1b0d6e83")
        );
    }

    #[test]
    fn reports_no_boot_id_for_an_empty_file() {
        // An empty id must never compare equal to an absent label, which would
        // read an overlay staged in this boot as predating it.
        assert_eq!(parse_boot_id(""), None);
    }

    #[test]
    fn reports_no_boot_id_for_whitespace_alone() {
        assert_eq!(parse_boot_id("\n"), None);
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
