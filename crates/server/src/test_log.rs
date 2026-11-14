// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The log records of this crate's tests, for a test to check what the server said, whichever process it runs in and whatever ran before it.

/// Every log record of this process's tests, so that a test can find its own.
struct Recorder(std::sync::Mutex<Vec<(log::Level, String)>>);

impl log::Log for Recorder {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        self.0
            .lock()
            .unwrap()
            .push((record.level(), record.args().to_string()));
    }

    fn flush(&self) {}
}

static LOG: Recorder = Recorder(std::sync::Mutex::new(Vec::new()));

/// Record every log record from now on in this process, at every level, so that each log line's arguments are evaluated and what it says can be checked.
pub fn record() {
    static RECORDING: std::sync::Once = std::sync::Once::new();
    RECORDING.call_once(|| {
        log::set_logger(&LOG).expect("only these tests log to a recorder");
        log::set_max_level(log::LevelFilter::Trace);
    });
}

/// The records logged since the first call to this or [`record`] that mention `about`. Tests run in parallel in one process too, so a test looks for what only it would say.
pub fn logged(about: &str) -> Vec<(log::Level, String)> {
    record();
    log::Log::flush(&LOG);
    assert!(log::Log::enabled(&LOG, &log::Metadata::builder().build()));
    LOG.0
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, line)| line.contains(about))
        .cloned()
        .collect()
}
