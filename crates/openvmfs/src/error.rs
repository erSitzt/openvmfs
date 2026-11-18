//! Error type shared by all layers.

use std::fmt;

/// What went wrong, in terms a caller can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The underlying device or file could not be read.
    Io,
    /// The on-disk data contradicts the format (bad magic, impossible offset, broken reference).
    Corrupt,
    /// The data may be valid, but uses something this library has not verified and therefore refuses to read.
    ///
    /// Callers that have another way to read the data should fall back to it.
    Unsupported,
    /// A file, directory or volume does not exist.
    NotFound,
    /// The caller passed something invalid (a path that is not a directory, a read beyond the end of a file).
    InvalidInput,
    /// An ESXi host had the file open for writing when the image of the datastore was taken. What the image holds
    /// of it may be out of date, and out of order: newer writes can be present while older ones are missing.
    ///
    /// For a virtual disk: take a VM snapshot before the storage snapshot and read the disk below it.
    InUse,
}

impl ErrorKind {
    fn label(self) -> &'static str {
        match self {
            ErrorKind::Io => "I/O error",
            ErrorKind::Corrupt => "corrupt data",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::NotFound => "not found",
            ErrorKind::InvalidInput => "invalid input",
            ErrorKind::InUse => "in use",
        }
    }
}

/// An error with its [`ErrorKind`] and a message that names the structure, field and offset involved.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    source: Option<std::io::Error>,
}

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Creates an error of the given kind.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
            source: None,
        }
    }

    /// The on-disk data contradicts the format.
    pub fn corrupt(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Corrupt, message)
    }

    /// The data uses something that has not been verified and is refused.
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unsupported, message)
    }

    /// A host had the file open for writing when the image was taken.
    pub fn in_use(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InUse, message)
    }

    /// Something does not exist.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    /// The caller passed something invalid.
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidInput, message)
    }

    /// A read from the device failed.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error {
            kind: ErrorKind::Io,
            message: context.into(),
            source: Some(source),
        }
    }

    /// The kind of this error.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The message without the kind prefix.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the same error with `context` put in front of its message.
    #[must_use]
    pub fn context(mut self, context: impl fmt::Display) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.label(), self.message)?;
        if let Some(source) = &self.source {
            write!(f, " ({source})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| e as &(dyn std::error::Error + 'static))
    }
}

/// Adds context to the error of a `Result`.
pub(crate) trait ResultExt<T> {
    fn context(self, context: impl fmt::Display) -> Result<T>;
}

impl<T> ResultExt<T> for Result<T> {
    fn context(self, context: impl fmt::Display) -> Result<T> {
        self.map_err(|e| e.context(context))
    }
}
