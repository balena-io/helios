//! System identification, as reported by `uname`

use nix::sys::utsname;

/// The system information reported by `uname`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uname {
    sysname: String,
    release: String,
    machine: String,
}

impl Uname {
    fn new(
        sysname: impl Into<String>,
        release: impl Into<String>,
        machine: impl Into<String>,
    ) -> Self {
        Self {
            sysname: sysname.into(),
            release: release.into(),
            machine: machine.into(),
        }
    }

    /// The kernel name, as given by `uname -s`
    pub fn sysname(&self) -> &str {
        &self.sysname
    }

    /// The kernel release, as given by `uname -r`
    pub fn release(&self) -> &str {
        &self.release
    }

    /// The hardware identifier, as given by `uname -m`
    pub fn machine(&self) -> &str {
        &self.machine
    }
}

/// Read the system information
///
/// This is a synchronous function. For an asynchronous version, wrap this
/// function in [`run_async`](crate::fs::run_async).
pub fn uname() -> std::io::Result<Uname> {
    let info = utsname::uname()?;
    Ok(Uname::new(
        info.sysname().to_string_lossy(),
        info.release().to_string_lossy(),
        info.machine().to_string_lossy(),
    ))
}
