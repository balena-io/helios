use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

/// The architectures supported by balena
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeviceArch {
    Amd64,
    Aarch64,
    Armv7hf,
    Rpi,
    I386,
    /// An architecture not known to balena, holding the machine name as reported by the system
    Other(String),
}

impl DeviceArch {
    /// The balena architecture slug
    pub fn as_str(&self) -> &str {
        match self {
            Self::Amd64 => "amd64",
            Self::Aarch64 => "aarch64",
            Self::Armv7hf => "armv7hf",
            Self::Rpi => "rpi",
            Self::I386 => "i386",
            Self::Other(machine) => machine,
        }
    }
}

impl fmt::Display for DeviceArch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DeviceArch {
    type Err = Infallible;

    /// Map a machine name, as given by `uname -m`, to the balena architecture.
    /// Unknown machines map to [`DeviceArch::Other`].
    fn from_str(machine: &str) -> Result<Self, Self::Err> {
        Ok(match machine {
            "x86_64" => Self::Amd64,
            "arm64" | "aarch64" => Self::Aarch64,
            "armv7l" => Self::Armv7hf,
            "armv6l" => Self::Rpi,
            "i686" | "i386" => Self::I386,
            machine => Self::Other(machine.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_machine_to_the_architecture() {
        for (machine, arch) in [
            ("x86_64", DeviceArch::Amd64),
            ("arm64", DeviceArch::Aarch64),
            ("aarch64", DeviceArch::Aarch64),
            ("armv7l", DeviceArch::Armv7hf),
            ("armv6l", DeviceArch::Rpi),
            ("i686", DeviceArch::I386),
            ("i386", DeviceArch::I386),
        ] {
            assert_eq!(machine.parse::<DeviceArch>(), Ok(arch), "{machine}");
        }
    }

    #[test]
    fn maps_an_unknown_machine_to_other() {
        assert_eq!(
            "riscv64".parse::<DeviceArch>(),
            Ok(DeviceArch::Other("riscv64".to_string()))
        );
    }
}
