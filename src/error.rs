use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CockpitError {
    #[error("PTY error: {0}")]
    Pty(String),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Render error: {0}")]
    Render(String),
    #[error("Metal error: {0}")]
    Metal(String),
    #[error("Glyph error: {0}")]
    Glyph(String),
    #[error("Config error: {0}")]
    Config(String),
    #[error("I/O error at {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("Session error: {0}")]
    Session(String),
    #[error("Sidebar error: {0}")]
    Sidebar(String),
}

pub type Result<T> = std::result::Result<T, CockpitError>;
