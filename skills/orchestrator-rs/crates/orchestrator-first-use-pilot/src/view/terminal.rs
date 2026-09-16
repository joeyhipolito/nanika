use std::io::{self, IsTerminal, Read, Write};
use std::time::Duration;

#[cfg(not(target_os = "macos"))]
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::termios::{
    LocalModes, OptionalActions, SpecialCodeIndex, Termios, tcgetattr, tcgetwinsize, tcsetattr,
};

pub(crate) const ENTER_SCREEN: &str = "\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H";
pub(crate) const LEAVE_SCREEN: &str = "\x1b[0m\x1b[?25h\x1b[?1049l";

pub(crate) struct TerminalGuard<'a, W: Write> {
    original: Termios,
    output: &'a mut W,
}

impl<'a, W: Write> TerminalGuard<'a, W> {
    pub(crate) fn enter(output: &'a mut W) -> io::Result<Self> {
        let input = io::stdin();
        let terminal_output = io::stdout();
        if !input.is_terminal() || !terminal_output.is_terminal() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "view requires terminal stdin and stdout; redirected streams are refused",
            ));
        }
        let original = tcgetattr(&input)?;
        let mut raw = original.clone();
        raw.make_raw();
        // Preserve signal generation so the installed SIGINT/SIGTERM handler
        // can unwind through this guard and restore the terminal.
        raw.local_modes.insert(LocalModes::ISIG);
        // macOS poll(2) does not support terminal devices. VTIME provides a
        // bounded direct-read path there and a safe readiness-race fallback.
        raw.special_codes[SpecialCodeIndex::VMIN] = 0;
        raw.special_codes[SpecialCodeIndex::VTIME] = 1;
        tcsetattr(&input, OptionalActions::Now, &raw)?;
        if let Err(error) = write!(output, "{ENTER_SCREEN}").and_then(|()| output.flush()) {
            let _ = tcsetattr(&input, OptionalActions::Now, &original);
            let _ = write!(output, "{LEAVE_SCREEN}");
            let _ = output.flush();
            return Err(error);
        }
        Ok(Self { original, output })
    }
}

impl<W: Write> Write for TerminalGuard<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

impl<W: Write> Drop for TerminalGuard<'_, W> {
    fn drop(&mut self) {
        let input = io::stdin();
        let _ = tcsetattr(&input, OptionalActions::Now, &self.original);
        let _ = write!(self.output, "{LEAVE_SCREEN}");
        let _ = self.output.flush();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Key {
    Character(char),
    Up,
    Down,
    PageUp,
    PageDown,
    Enter,
    Tab,
    Escape,
    Backspace,
    Interrupt,
}

pub(crate) fn dimensions() -> (usize, usize) {
    let output = io::stdout();
    match tcgetwinsize(&output) {
        Ok(size) => (
            usize::from(size.ws_col).clamp(1, 300),
            usize::from(size.ws_row).clamp(1, 120),
        ),
        Err(_) => (80, 24),
    }
}

#[derive(Default)]
pub(crate) struct Input {
    pending: Vec<u8>,
}

impl Input {
    pub(crate) fn read_key(&mut self, timeout: Duration) -> io::Result<Option<Key>> {
        if let Some(key) = self.take_key(false) {
            return Ok(Some(key));
        }
        let input = io::stdin();
        #[cfg(not(target_os = "macos"))]
        {
            let wait = Timespec::try_from(timeout).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid input timeout")
            })?;
            let mut descriptors = [PollFd::new(&input, PollFlags::IN)];
            match poll(&mut descriptors, Some(&wait)) {
                Ok(0) => return Ok(self.take_key(true)),
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => return Ok(None),
                Err(error) => return Err(error.into()),
            }
            if descriptors[0]
                .revents()
                .intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL)
            {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "terminal input failed",
                ));
            }
        }
        #[cfg(target_os = "macos")]
        let _timeout = timeout;
        let mut bytes = [0u8; 64];
        let count = match input.lock().read(&mut bytes) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(None),
            Err(error) => return Err(error),
        };
        self.pending.extend_from_slice(&bytes[..count]);
        Ok(self.take_key(count == 0))
    }

    fn take_key(&mut self, escape_timeout: bool) -> Option<Key> {
        let (key, count) = decode_key(&self.pending, escape_timeout)?;
        self.pending.drain(..count);
        Some(key)
    }
}

fn decode_key(bytes: &[u8], escape_timeout: bool) -> Option<(Key, usize)> {
    let first = *bytes.first()?;
    if first == 0x1b {
        let sequences: &[(&[u8], Key)] = &[
            (b"\x1b[A", Key::Up),
            (b"\x1b[B", Key::Down),
            (b"\x1b[5~", Key::PageUp),
            (b"\x1b[6~", Key::PageDown),
        ];
        for (sequence, key) in sequences {
            if bytes.starts_with(sequence) {
                return Some((*key, sequence.len()));
            }
        }
        if sequences
            .iter()
            .any(|(sequence, _)| sequence.starts_with(bytes))
            && !(escape_timeout && bytes.len() == 1)
        {
            return None;
        }
        return Some((Key::Escape, 1));
    }
    let key = match first {
        b'\r' | b'\n' => Key::Enter,
        b'\t' => Key::Tab,
        0x7f | 0x08 => Key::Backspace,
        0x03 => Key::Interrupt,
        _ => {
            let width = match first {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => 1,
            };
            if bytes.len() < width {
                return None;
            }
            return match std::str::from_utf8(&bytes[..width])
                .ok()
                .and_then(|s| s.chars().next())
            {
                Some(character) => Some((Key::Character(character), width)),
                None => Some((Key::Character('�'), 1)),
            };
        }
    };
    Some((key, 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batched_keys_and_split_unicode_and_arrows_are_retained() {
        let mut input = Input::default();
        input.pending.extend_from_slice(b"jq");
        assert_eq!(input.take_key(false), Some(Key::Character('j')));
        assert_eq!(input.take_key(false), Some(Key::Character('q')));
        input.pending.extend_from_slice(b"\x1b[");
        assert_eq!(input.take_key(false), None);
        input.pending.extend_from_slice(b"A\xc3");
        assert_eq!(input.take_key(false), Some(Key::Up));
        assert_eq!(input.take_key(false), None);
        input.pending.extend_from_slice(b"\xa9\r");
        assert_eq!(input.take_key(false), Some(Key::Character('é')));
        assert_eq!(input.take_key(false), Some(Key::Enter));
        assert!(input.pending.is_empty());
    }

    #[test]
    fn key_decoder_recognizes_navigation_without_accepting_escape_text() {
        assert_eq!(decode_key(b"\x1b[A", false), Some((Key::Up, 3)));
        assert_eq!(decode_key(b"\x1b[6~", false), Some((Key::PageDown, 4)));
    }
}
