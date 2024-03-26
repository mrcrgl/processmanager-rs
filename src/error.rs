
#[derive(thiserror::Error, Debug)]
pub enum RuntimeError {
    #[error("internal error: {message}")]
    Internal { message: String },

    #[error("process termination requested")]
    TerminationSignal,
}

