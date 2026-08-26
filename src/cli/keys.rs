//! Raw mode và giải mã phím cho màn hình onboarding. Chỉ chạy trên unix.

use std::io;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Up,
    Down,
    Space,
    Enter,
    Quit,
    Interrupt,
    Other,
}

/// Giải mã phím đầu tiên trong buffer, trả kèm số byte đã dùng.
///
/// `None` nghĩa là chuỗi escape còn dở, người gọi cần đọc thêm byte.
pub fn decode(bytes: &[u8]) -> Option<(Key, usize)> {
    match bytes {
        [] | [0x1b] | [0x1b, b'['] => None,
        [0x1b, b'[', b'A', ..] => Some((Key::Up, 3)),
        [0x1b, b'[', b'B', ..] => Some((Key::Down, 3)),
        [0x1b, b'[', _, ..] => Some((Key::Other, 3)),
        [0x1b, ..] => Some((Key::Other, bytes.len())),
        [b' ', ..] => Some((Key::Space, 1)),
        [b'\r' | b'\n', ..] => Some((Key::Enter, 1)),
        [0x03, ..] => Some((Key::Interrupt, 1)),
        [b'q' | b'Q', ..] => Some((Key::Quit, 1)),
        [_, ..] => Some((Key::Other, 1)),
    }
}

/// Đặt stdin vào raw mode và trả về guard restore lại khi `Drop`.
///
/// Guard bảo đảm terminal không bị bỏ ở trạng thái raw kể cả khi panic.
pub struct RawMode {
    descriptor: i32,
    original: libc::termios,
}

impl RawMode {
    /// `Ok(None)` khi stdin không phải terminal - người gọi phải đi đường không tương tác.
    pub fn enable() -> io::Result<Option<Self>> {
        let descriptor = libc::STDIN_FILENO;
        if unsafe { libc::isatty(descriptor) } != 1 {
            return Ok(None);
        }
        let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(descriptor, original.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let original = unsafe { original.assume_init() };
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(descriptor, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(Self {
            descriptor,
            original,
        }))
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(self.descriptor, libc::TCSANOW, &self.original) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_arrow_keys() {
        assert_eq!(decode(b"\x1b[A"), Some((Key::Up, 3)));
        assert_eq!(decode(b"\x1b[B"), Some((Key::Down, 3)));
    }

    #[test]
    fn decodes_the_action_keys() {
        assert_eq!(decode(b" "), Some((Key::Space, 1)));
        assert_eq!(decode(b"\r"), Some((Key::Enter, 1)));
        assert_eq!(decode(b"\n"), Some((Key::Enter, 1)));
        assert_eq!(decode(b"q"), Some((Key::Quit, 1)));
        assert_eq!(decode(b"Q"), Some((Key::Quit, 1)));
        assert_eq!(decode(b"\x03"), Some((Key::Interrupt, 1)));
    }

    #[test]
    fn waits_for_more_bytes_on_a_partial_escape_sequence() {
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"\x1b"), None);
        assert_eq!(decode(b"\x1b["), None);
    }

    #[test]
    fn consumes_only_the_first_key_of_a_burst() {
        assert_eq!(decode(b"\x1b[Aq"), Some((Key::Up, 3)));
        assert_eq!(decode(b" \r"), Some((Key::Space, 1)));
    }

    #[test]
    fn maps_an_unhandled_byte_to_other() {
        assert_eq!(decode(b"x"), Some((Key::Other, 1)));
        assert_eq!(decode(b"\x1b[C"), Some((Key::Other, 3)));
    }
}
