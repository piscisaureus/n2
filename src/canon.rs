//! Path canonicalization.

use std::ffi::OsString;
use std::mem::replace;
use std::mem::take;
use std::mem::MaybeUninit;

use crate::byte_string::*;

/// An on-stack stack of values.
/// Used for tracking locations of parent components within a path.
struct StackStack<T> {
    n: usize,
    vals: [MaybeUninit<T>; 60],
}

impl<T: Copy> StackStack<T> {
    fn new() -> Self {
        StackStack {
            n: 0,
            // Safety: we only access vals[i] after setting it.
            vals: unsafe { MaybeUninit::uninit().assume_init() },
        }
    }

    fn push(&mut self, val: T) {
        if self.n >= self.vals.len() {
            panic!("too many path components");
        }
        self.vals[self.n].write(val);
        self.n += 1;
    }

    fn pop(&mut self) -> Option<T> {
        if self.n > 0 {
            self.n -= 1;
            // Safety: we only access vals[i] after setting it.
            Some(unsafe { self.vals[self.n].assume_init() })
        } else {
            None
        }
    }
}

/// Lexically canonicalize a path, removing redundant components.
/// Does not access the disk, but only simplifies things like
/// "foo/./bar" => "foo/bar".
/// These paths can show up due to variable expansion in particular.
pub fn canon_path_in_place(path_buf: &mut OsString) {
    let mut byte_buf = take(path_buf).into_byte_string();

    // Safety: this traverses the path buffer to move data around.
    // We maintain the invariant that *dst always points to a point within
    // the buffer, and that src is always checked against end before reading.
    unsafe {
        let mut components = StackStack::<*mut u8>::new();
        let mut dst = byte_buf.as_mut_ptr();
        let mut src = byte_buf.as_ptr();
        let end = src.add(byte_buf.len());

        if src == end {
            return;
        }
        if *src == b'/' {
            src = src.add(1);
            dst = dst.add(1);
        }

        // Outer loop: one iteration per path component.
        while src < end {
            // Peek ahead for special path components: "/", ".", and "..".
            match *src {
                b'/' => {
                    src = src.add(1);
                    continue;
                }
                b'.' => {
                    let mut peek = src.add(1);
                    if peek == end {
                        break; // Trailing '.', trim.
                    }
                    match *peek {
                        b'/' => {
                            // "./", skip.
                            src = src.add(2);
                            continue;
                        }
                        b'.' => {
                            // ".."
                            peek = peek.add(1);
                            if !(peek == end || *peek == b'/') {
                                // Componet that happens to start with "..".
                                // Handle as an ordinary component.
                                break;
                            }
                            // ".." component, try to back up.
                            if let Some(ofs) = components.pop() {
                                dst = ofs;
                            } else {
                                *dst = b'.';
                                dst = dst.add(1);
                                *dst = b'.';
                                dst = dst.add(1);
                                if peek != end {
                                    *dst = b'/';
                                    dst = dst.add(1);
                                }
                            }
                            src = src.add(3);
                            continue;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }

            // Mark this point as a possible target to pop to.
            components.push(dst);

            // Inner loop: copy one path component, including trailing '/'.
            while src < end {
                *dst = *src;
                src = src.add(1);
                dst = dst.add(1);
                if *src.offset(-1) == b'/' {
                    break;
                }
            }
        }

        byte_buf.set_len(dst.offset_from(byte_buf.as_ptr()) as usize);

        let temp = replace(path_buf, byte_buf.into_os_string().unwrap());
        assert!(temp.is_empty());
    }
}

pub fn canon_path(path: impl Into<OsString>) -> OsString {
    let mut path_buf = path.into();
    canon_path_in_place(&mut path_buf);
    path_buf
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn noop() {
        assert_eq!(canon_path("foo"), OsStr::new("foo"));
        assert_eq!(canon_path("foo/bar"), OsStr::new("foo/bar"));
    }

    #[test]
    fn dot() {
        assert_eq!(canon_path("./foo"), OsStr::new("foo"));
        assert_eq!(canon_path("foo/."), OsStr::new("foo/"));
        assert_eq!(canon_path("foo/./bar"), OsStr::new("foo/bar"));
    }

    #[test]
    fn slash() {
        assert_eq!(canon_path("/foo"), OsStr::new("/foo"));
        assert_eq!(canon_path("foo//bar"), OsStr::new("foo/bar"));
    }

    #[test]
    fn parent() {
        assert_eq!(canon_path("foo/../bar"), OsStr::new("bar"));
        assert_eq!(canon_path("/foo/../bar"), OsStr::new("/bar"));
        assert_eq!(canon_path("../foo"), OsStr::new("../foo"));
        assert_eq!(canon_path("../foo/../bar"), OsStr::new("../bar"));
        assert_eq!(canon_path("../../bar"), OsStr::new("../../bar"));
    }
}
