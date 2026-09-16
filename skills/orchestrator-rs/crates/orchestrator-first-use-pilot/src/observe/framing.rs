//! Bounded physical-line framing; a rejected prefix stays rejected to newline.

pub(crate) struct Line {
    pub bytes: Vec<u8>,
    pub start: u64,
    pub end: u64,
    pub truncated: bool,
}

#[derive(Clone, Default)]
pub(crate) struct Framer {
    bytes: Vec<u8>,
    start: u64,
    position: u64,
    discarding: bool,
}

impl Framer {
    pub fn pending(&self) -> bool {
        self.position != self.start
    }

    pub fn lose_sync(&mut self) {
        self.bytes.clear();
        self.discarding = true;
    }

    pub fn push(&mut self, byte: u8) -> Option<Line> {
        self.position = self.position.saturating_add(1);
        if byte == b'\n' {
            let line = self.take();
            return Some(line);
        }
        if !self.discarding {
            if self.bytes.len() == super::MAX_FRAME_BYTES {
                self.bytes.clear();
                self.discarding = true;
            } else {
                self.bytes.push(byte);
            }
        }
        None
    }

    pub fn finish(&mut self) -> Option<Line> {
        self.pending().then(|| self.take())
    }

    fn take(&mut self) -> Line {
        let line = Line {
            bytes: std::mem::take(&mut self.bytes),
            start: self.start,
            end: self.position,
            truncated: self.discarding,
        };
        self.start = self.position;
        self.discarding = false;
        line
    }
}
