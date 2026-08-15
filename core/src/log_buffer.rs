use core::fmt::{self, Write};

const HEADER_LEN: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
}

impl Level {
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Info),
            1 => Some(Self::Warn),
            _ => None,
        }
    }
}

pub struct LogBuffer<const N: usize> {
    bytes: [u8; N],
    len: usize,
    dropped: usize,
}

impl<const N: usize> LogBuffer<N> {
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
            dropped: 0,
        }
    }

    pub fn info(&mut self, args: fmt::Arguments<'_>) {
        self.record(Level::Info, args);
    }

    pub fn warn(&mut self, args: fmt::Arguments<'_>) {
        self.record(Level::Warn, args);
    }

    pub fn record(&mut self, level: Level, args: fmt::Arguments<'_>) {
        let start = self.len;

        if N - start < HEADER_LEN {
            self.dropped += 1;
            return;
        }

        self.len += HEADER_LEN;

        let mut sink = Sink {
            buffer: self,
            overflowed: false,
        };
        let _ = sink.write_fmt(args);
        let overflowed = sink.overflowed;

        if overflowed {
            self.len = start;
            self.dropped += 1;
            return;
        }

        let text_len = self.len - start - HEADER_LEN;
        self.bytes[start] = text_len as u8;
        self.bytes[start + 1] = (text_len >> 8) as u8;
        self.bytes[start + 2] = level as u8;
    }

    pub fn entries(&self) -> Entries<'_> {
        Entries {
            bytes: &self.bytes[..self.len],
            offset: 0,
        }
    }

    pub fn dropped(&self) -> usize {
        self.dropped
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.dropped = 0;
    }
}

impl<const N: usize> Default for LogBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

struct Sink<'a, const N: usize> {
    buffer: &'a mut LogBuffer<N>,
    overflowed: bool,
}

impl<const N: usize> Write for Sink<'_, N> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let bytes = text.as_bytes();
        let end = self.buffer.len + bytes.len();

        if end > N {
            self.overflowed = true;
            return Err(fmt::Error);
        }

        self.buffer.bytes[self.buffer.len..end].copy_from_slice(bytes);
        self.buffer.len = end;

        Ok(())
    }
}

pub struct Entries<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Iterator for Entries<'a> {
    type Item = (Level, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset + HEADER_LEN > self.bytes.len() {
            return None;
        }

        let text_len =
            u16::from_le_bytes([self.bytes[self.offset], self.bytes[self.offset + 1]]) as usize;
        let level = Level::from_byte(self.bytes[self.offset + 2])?;
        let start = self.offset + HEADER_LEN;
        let end = start + text_len;
        let text = core::str::from_utf8(self.bytes.get(start..end)?).ok()?;

        self.offset = end;

        Some((level, text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_replay_in_order_with_their_levels() {
        let mut buffer = LogBuffer::<128>::new();

        buffer.info(format_args!("woke on {}", "timer"));
        buffer.warn(format_args!("temperature unavailable: {}", "timeout"));

        let entries: Vec<_> = buffer.entries().collect();

        assert_eq!(
            entries,
            [
                (Level::Info, "woke on timer"),
                (Level::Warn, "temperature unavailable: timeout"),
            ]
        );
    }

    #[test]
    fn a_new_buffer_is_empty() {
        let buffer = LogBuffer::<32>::new();

        assert!(buffer.is_empty());
        assert_eq!(buffer.entries().count(), 0);
        assert_eq!(buffer.dropped(), 0);
    }

    #[test]
    fn an_entry_that_does_not_fit_is_dropped_whole() {
        let mut buffer = LogBuffer::<32>::new();

        buffer.info(format_args!("kept"));
        buffer.info(format_args!("{:40}", "too long"));

        let entries: Vec<_> = buffer.entries().collect();

        assert_eq!(entries, [(Level::Info, "kept")]);
        assert_eq!(buffer.dropped(), 1);
    }

    #[test]
    fn recording_continues_after_a_dropped_entry() {
        let mut buffer = LogBuffer::<32>::new();

        buffer.info(format_args!("{:40}", "too long"));
        buffer.info(format_args!("kept"));

        let entries: Vec<_> = buffer.entries().collect();

        assert_eq!(entries, [(Level::Info, "kept")]);
        assert_eq!(buffer.dropped(), 1);
    }

    #[test]
    fn clearing_discards_entries_and_the_dropped_count() {
        let mut buffer = LogBuffer::<32>::new();

        buffer.info(format_args!("{:40}", "too long"));
        buffer.info(format_args!("kept"));
        buffer.clear();

        assert!(buffer.is_empty());
        assert_eq!(buffer.entries().count(), 0);
        assert_eq!(buffer.dropped(), 0);
    }

    #[test]
    fn multibyte_text_survives_the_round_trip() {
        let mut buffer = LogBuffer::<64>::new();

        buffer.info(format_args!("17.250 °C"));

        assert_eq!(buffer.entries().next(), Some((Level::Info, "17.250 °C")));
    }
}
