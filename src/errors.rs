use std::io;

#[derive(Debug)]
pub enum ParseError {
    Io(io::Error),
    Message(String),
    File(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::File(s) => write!(f, "File open error: {}", s),
            ParseError::Io(e) => write!(f, "IO error: {}", e),
            ParseError::Message(s) => write!(f, "Parse error: {}", s),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<io::Error> for ParseError {
    fn from(e: io::Error) -> Self {
        ParseError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, ParseError>;
