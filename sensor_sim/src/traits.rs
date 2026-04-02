use std::time::{Duration, Instant};

/// Contract for a simulated hardware sensor with an internal sample buffer.
pub trait Sensor {
    /// Reading type produced by this sensor (e.g. temperature, acceleration).
    type SensorReading;

    /// Builds a new sensor instance without starting its producer thread yet.
    ///
    /// # Arguments
    ///
    /// * `id` — Stable identifier used in logs and aggregation.
    /// * `rate_per_sec` — Target samples per second for the background producer.
    ///
    /// # Returns
    ///
    /// A new `Self` in a stopped or idle state until [`Sensor::start`] is called.
    fn new(id: String, rate_per_sec: u32) -> Self;

    /// Starts (or resumes) background generation of readings into the internal buffer.
    ///
    /// # Arguments
    ///
    /// * `self` — Mutable sensor state.
    ///
    /// # Returns
    ///
    /// `()`.
    fn start(&mut self);

    /// Removes and returns one reading from the internal buffer, if any.
    ///
    /// # Arguments
    ///
    /// * `self` — Sensor shared reference (read side).
    ///
    /// # Returns
    ///
    /// `Some(reading)` or `None` when the buffer is empty.
    fn read(&self) -> Option<Self::SensorReading>;

    /// Number of readings currently waiting in the sensor’s internal queue.
    ///
    /// # Arguments
    ///
    /// * `self` — Sensor shared reference.
    ///
    /// # Returns
    ///
    /// Queue depth; if it stays at capacity, newer samples may be dropped or overwritten.
    fn available(&self) -> usize;

    /// Copies the sensor’s configured identifier.
    ///
    /// # Arguments
    ///
    /// * `self` — Sensor shared reference.
    ///
    /// # Returns
    ///
    /// Owned `String` with the sensor id.
    fn id(&self) -> String;

    /// Stops the background producer and joins any internal threads as needed.
    ///
    /// # Arguments
    ///
    /// * `self` — Mutable sensor state.
    ///
    /// # Returns
    ///
    /// `()`.
    fn stop(&mut self);

    /// Blocks until at least one sample is available or `timeout` elapses.
    ///
    /// # Arguments
    ///
    /// * `self` — Sensor shared reference.
    /// * `timeout` — Maximum time to wait before giving up.
    ///
    /// # Returns
    ///
    /// `true` if `available() > 0` before timeout, otherwise `false`.
    ///
    /// Default implementation polls every millisecond; implementations may override with condvars.
    fn wait_for_data(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        loop {
            if self.available() > 0 {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
