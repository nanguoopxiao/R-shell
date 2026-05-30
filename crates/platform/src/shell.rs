#[must_use]
pub fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("ComSpec").unwrap_or_else(|_| "powershell.exe".to_string())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

#[must_use]
pub fn default_shell_args() -> Vec<String> {
    if cfg!(windows) && default_shell().to_ascii_lowercase().contains("powershell") {
        vec!["-NoLogo".to_string()]
    } else {
        Vec::new()
    }
}
