use std::path::PathBuf;

/// Return the application configuration directory
pub fn config_dir() -> PathBuf {
    let dir = if let Some(config_dir) = dirs::config_dir() {
        config_dir
    } else {
        // Fallback to home directory if config dir is not available
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config")
    };
    dir.join(env!("HELIOS_PKG_NAME"))
}

/// Return the application state directory
pub fn state_dir() -> PathBuf {
    let dir = if let Some(state_dir) = dirs::state_dir() {
        state_dir
    } else {
        // Fallback to home directory if state dir is not available
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local")
            .join("state")
    };
    dir.join(env!("HELIOS_PKG_NAME"))
}

/// Return the runtime directory for temporary files
pub fn runtime_dir() -> PathBuf {
    if let Some(runtime_dir) = dirs::runtime_dir() {
        runtime_dir
    } else {
        // Fallback to the OS temp dir
        std::env::temp_dir().join(env!("HELIOS_PKG_NAME"))
    }
}

pub fn ensure_runtime_dir() -> std::io::Result<()> {
    std::fs::create_dir_all(runtime_dir())?;
    Ok(())
}
