//! Compile time configurations for the util crate
fn main() {
    // the HELIOS_PKG_NAME variable is used to create local folders under
    // `~/.config`, `~/.local/state` and other directories. Do not change the name
    // unless you know what you are doing as this means any prior files will no longer
    // be accessible to the service.
    println!("cargo::rustc-env=HELIOS_PKG_NAME=helios");
}
