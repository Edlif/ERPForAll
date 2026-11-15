// SPDX-License-Identifier: MIT OR Apache-2.0
//! A worker's stderr, kept in memory up to a cap. The oldest bytes go first and are counted.
use std::{
    collections::VecDeque,
    io::Read,
    sync::{Mutex, PoisonError},
};

pub(crate) struct CappedLog {
    state: Mutex<State>,
    cap: usize,
}
struct State {
    bytes: VecDeque<u8>,
    dropped: u64,
}
impl CappedLog {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            state: Mutex::new(State {
                bytes: VecDeque::new(),
                dropped: 0,
            }),
            cap,
        }
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        // Bytes and a counter, whole after any interrupted push: a poisoned lock is taken over.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub(crate) fn push(&self, chunk: &[u8]) {
        let mut state = self.state();
        // A chunk larger than the cap keeps its tail only.
        let skipped = chunk.len().saturating_sub(self.cap);
        state.dropped += skipped as u64;
        let chunk = &chunk[skipped..];
        let overflow = (state.bytes.len() + chunk.len()).saturating_sub(self.cap);
        state.bytes.drain(..overflow);
        state.dropped += overflow as u64;
        state.bytes.extend(chunk);
    }
    /// Start the log of the next document.
    pub(crate) fn reset(&self) {
        let mut state = self.state();
        state.bytes.clear();
        state.dropped = 0;
    }
    /// The kept text and the number of bytes dropped before it.
    pub(crate) fn snapshot(&self) -> (String, u64) {
        let state = self.state();
        let bytes: Vec<u8> = state.bytes.iter().copied().collect();
        (String::from_utf8_lossy(&bytes).into_owned(), state.dropped)
    }
    /// The kept text with a first line that counts what was dropped, for an error's `log` field.
    pub(crate) fn text(&self) -> String {
        let (text, dropped) = self.snapshot();
        if dropped == 0 {
            text
        } else {
            format!("[{dropped} earlier bytes dropped]\n{text}")
        }
    }
    /// Read `source` to its end into the log. A failed read is written into the log itself.
    pub(crate) fn drain(&self, mut source: impl Read) {
        let mut chunk = [0u8; 4096];
        loop {
            match source.read(&mut chunk) {
                Ok(0) => return,
                Ok(read) => self.push(&chunk[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    self.push(format!("\n[worker log pipe failed: {error}]\n").as_bytes());
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_oldest_bytes_are_dropped_and_counted() {
        let log = CappedLog::new(8);
        log.push(b"abcdef");
        assert_eq!(log.snapshot(), ("abcdef".into(), 0));
        log.push(b"ghij");
        assert_eq!(log.snapshot(), ("cdefghij".into(), 2));
        log.push(b"0123456789AB");
        assert_eq!(log.snapshot(), ("456789AB".into(), 14));
        assert_eq!(log.text(), "[14 earlier bytes dropped]\n456789AB");
        log.reset();
        assert_eq!(log.snapshot(), (String::new(), 0));
        assert_eq!(log.text(), "");
    }

    #[test]
    fn a_source_is_read_to_its_end() {
        let log = CappedLog::new(1024);
        log.drain(&b"first line\nsecond line\n"[..]);
        assert_eq!(log.text(), "first line\nsecond line\n");
    }
}
