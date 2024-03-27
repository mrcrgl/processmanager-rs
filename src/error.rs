use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum RuntimeError {
    Internal { message: String },
    TerminationSignal,
}

impl Display for RuntimeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Internal { message } => write!(f, "internal error: {message}"),
            RuntimeError::TerminationSignal => write!(f, "process termination requested"),
        }
    }
}

impl ::std::error::Error for RuntimeError {}
