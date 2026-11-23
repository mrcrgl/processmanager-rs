use std::{
    error::Error,
    fmt::{Display, Formatter},
};

/// A boxed error that a [`RuntimeError::Failed`] carries as its source.
pub type BoxError = Box<dyn Error + Send + Sync + 'static>;

/// Error returned by a [`Runnable`](crate::Runnable) and propagated by
/// supervisors such as [`ProcessManager`](crate::ProcessManager).
///
/// The enum is `#[non_exhaustive]`: match it with a wildcard arm so new
/// variants can be added without a breaking change.
#[derive(Debug)]
#[non_exhaustive]
pub enum RuntimeError {
    /// A failure described only by a message, e.g. a panicking child or a
    /// child aborted after the shutdown grace period. Prefer
    /// [`RuntimeError::failed`] for errors that have a typed cause.
    Internal {
        /// Description of the failure.
        message: String,
    },
    /// The process asked for a graceful stop, e.g. a `SignalReceiver` after
    /// `SIGTERM`. A `ProcessManager` treats it as a shutdown request, not as a
    /// failure.
    TerminationSignal,
    /// A child failed with its own error.
    ///
    /// Transparent: `Display` and [`Error::source`] forward to the wrapped
    /// error. Use [`RuntimeError::downcast_ref`] to get the typed error back.
    Failed(BoxError),
}

impl RuntimeError {
    /// Wraps any error as [`RuntimeError::Failed`]; get it back with
    /// [`RuntimeError::downcast_ref`].
    ///
    /// ```
    /// # use processmanager::RuntimeError;
    /// let err = RuntimeError::failed(std::io::Error::other("disk full"));
    /// assert_eq!(err.to_string(), "disk full");
    /// ```
    pub fn failed(err: impl Into<BoxError>) -> Self {
        RuntimeError::Failed(err.into())
    }

    /// Returns the wrapped error of a [`RuntimeError::Failed`] if it is an `E`.
    ///
    /// ```
    /// # use processmanager::RuntimeError;
    /// let err = RuntimeError::failed(std::io::Error::other("disk full"));
    /// let io = err.downcast_ref::<std::io::Error>().unwrap();
    /// assert_eq!(io.kind(), std::io::ErrorKind::Other);
    /// ```
    pub fn downcast_ref<E: Error + 'static>(&self) -> Option<&E> {
        match self {
            RuntimeError::Failed(err) => err.downcast_ref::<E>(),
            _ => None,
        }
    }
}

impl Display for RuntimeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Internal { message } => write!(f, "internal error: {message}"),
            RuntimeError::TerminationSignal => write!(f, "process termination requested"),
            RuntimeError::Failed(err) => Display::fmt(err, f),
        }
    }
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            // Transparent: the wrapped error is this error's message, so its
            // *own* source is the next link in the chain.
            RuntimeError::Failed(err) => err.source(),
            _ => None,
        }
    }
}

/// Lets `?` propagate I/O errors from a `Runnable` body.
impl From<std::io::Error> for RuntimeError {
    fn from(err: std::io::Error) -> Self {
        RuntimeError::failed(err)
    }
}

impl From<BoxError> for RuntimeError {
    fn from(err: BoxError) -> Self {
        RuntimeError::Failed(err)
    }
}
