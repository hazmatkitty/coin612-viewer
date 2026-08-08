//! Custom SDL user events pushed from the USB reader thread.

/// A new frame has been published to the triple buffer.
pub struct NewFrame;

/// The reader thread died on a USB error; it has already exited.
pub struct Disconnected {
    pub msg: String,
}
