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
