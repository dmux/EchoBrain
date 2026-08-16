//! Where the speech model lives.
//!
//! The model is ~570 MB. It is deliberately **not** bundled: it would dominate
//! the app download, it changes on a different cadence from the code, and its
//! licence is not this project's to redistribute. `make model` fetches it.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("could not determine the application support directory")]
    NoHome,

    #[error(
        "the speech model is not installed at {0}.\n\
         Run `make model` to download it (~570 MB, once)."
    )]
    Missing(PathBuf),
}

/// `~/Library/Application Support/EchoBrain/models/<name>` on macOS.
pub fn model_path(name: &str) -> Result<PathBuf, ModelError> {
    let home = std::env::var_os("HOME").ok_or(ModelError::NoHome)?;
    let dir = if cfg!(target_os = "macos") {
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("EchoBrain")
            .join("models")
    } else {
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("echobrain")
            .join("models")
    };
    Ok(dir.join(name))
}

/// The path, checked. Separate from [`model_path`] so callers can report the
/// expected location even when nothing is there yet.
pub fn require_model(name: &str) -> Result<PathBuf, ModelError> {
    let path = model_path(name)?;
    if path.exists() {
        Ok(path)
    } else {
        Err(ModelError::Missing(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_lives_under_application_support_on_macos() {
        let path = model_path("ggml-tiny.bin").expect("HOME is set in tests");
        assert!(path.ends_with("EchoBrain/models/ggml-tiny.bin") || !cfg!(target_os = "macos"));
    }

    #[test]
    fn a_missing_model_reports_where_it_should_be() {
        let error = require_model("definitely-not-here.bin").expect_err("absent");
        let message = error.to_string();
        assert!(
            message.contains("make model"),
            "the error must say how to fix it, got: {message}"
        );
    }
}
