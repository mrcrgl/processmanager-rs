mod idle;
mod signal_receiver;

pub use idle::IdleProcess;
#[cfg(feature = "signal")]
pub use signal_receiver::SignalReceiver;
