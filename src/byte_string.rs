use std::alloc::Layout;
use std::borrow::Borrow;
use std::borrow::Cow;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::str::Utf8Error;
use std::string::FromUtf8Error;

pub type ByteString = Vec<u8>;
#[allow(non_camel_case_types)]
pub type bstr = [u8];

pub trait OwnedBytes: Borrow<Self::Slice> + Sized {
    type Slice: BorrowedBytes + ?Sized;

    fn into_byte_string(self) -> Vec<u8> {
        // This is safe because all "string" types are really just wrappers
        // around `Vec<u8>`. We need this hack because on Windows, Rust
        // prohibits access to the raw bytes that make up an OsString/PathBuf.
        unsafe { cast(self) }
    }

    fn into_string(self) -> Result<String, FromUtf8Error> {
        let byte_string = self.into_byte_string();
        let s = String::from_utf8(byte_string)?;
        Ok(s)
    }

    #[cfg(unix)]
    fn into_os_string(self) -> Result<OsString, FromUtf8Error> {
        use std::os::unix::ffi::OsStringExt;
        let s = self.into_byte_string();
        let s = OsString::from_vec(s);
        Ok(s)
    }

    #[cfg(windows)]
    fn into_os_string(self) -> Result<OsString, FromUtf8Error> {
        // TODO: consider exposing WTF8 on Windows.
        let s = self.into_string()?;
        let s = OsString::from(s);
        Ok(s)
    }

    fn into_path_buf(self) -> Result<PathBuf, FromUtf8Error> {
        let s = self.into_os_string()?;
        let s = PathBuf::from(s);
        Ok(s)
    }

    fn to_string_lossy(&self) -> String {
        Borrow::<Self::Slice>::borrow(self).as_str_lossy().into()
    }
}

pub trait BorrowedBytes {
    fn as_bstr(&self) -> &[u8] {
        // This is safe because all 'string slice' types are simple wrappers
        // around byte slices (`&[u8]`).
        // Also see comments by `OwnedBytes::into_byte_string()`.
        unsafe { cast(self) }
    }

    fn as_str(&self) -> Result<&str, Utf8Error> {
        let s = self.as_bstr();
        let s = std::str::from_utf8(s)?;
        Ok(s)
    }

    #[cfg(unix)]
    fn as_os_str(&self) -> Result<&OsStr, Utf8Error> {
        use std::os::unix::ffi::OsStrExt;
        let s = self.as_bstr();
        Ok(OsStr::from_bytes(s))
    }

    #[cfg(windows)]
    fn as_os_str(&self) -> Result<&OsStr, Utf8Error> {
        // TODO: consider exposing WTF8 on Windows.
        let s = self.as_str()?;
        Ok(s.as_ref())
    }

    fn as_path(&self) -> Result<&Path, Utf8Error> {
        let s = self.as_os_str()?;
        Ok(s.as_ref())
    }

    fn to_byte_string(&self) -> Vec<u8> {
        self.as_bstr().to_owned()
    }

    fn to_string(&self) -> Result<String, Utf8Error> {
        let s = self.as_str()?;
        Ok(s.to_owned())
    }

    fn to_os_string(&self) -> Result<OsString, Utf8Error> {
        let s = self.as_os_str()?;
        Ok(s.to_owned())
    }

    fn to_path_buf(&self) -> Result<PathBuf, Utf8Error> {
        let s = self.as_path()?;
        Ok(s.to_owned())
    }

    fn as_str_lossy(&self) -> Cow<str> {
        let b = self.as_bstr();
        String::from_utf8_lossy(b)
    }

    // Note that there is no `as_bytes_mut()` method, since this cannot be
    // done safely. Mutating the underlying bytes might invalidate invariants
    // that need to be upheld, namely that `String`s contain only valid UTF-8,
    // and that `OsString`s and `Path`s need to be valid WTF-8 on Windows.
}

// `OsString` and `PathBuf` are really just wrappers around `Vec<u8>`. However
// Rust doesn't allow us access to the raw bytes on Windows because it somehow
// believes WTF-8 encoded strings should not be accessible.
impl OwnedBytes for ByteString {
    type Slice = bstr;
}
impl OwnedBytes for String {
    type Slice = str;
}
impl OwnedBytes for OsString {
    type Slice = OsStr;
}
impl OwnedBytes for PathBuf {
    type Slice = Path;
}

// `OsStr` and `Path` are really just wrappers around `[u8]`.
impl BorrowedBytes for bstr {}
impl BorrowedBytes for str {}
impl BorrowedBytes for OsStr {}
impl BorrowedBytes for Path {}

unsafe fn cast<From: Sized, To: Sized>(value: From) -> To {
    assert_eq!(Layout::new::<From>(), Layout::new::<To>());
    let value = ManuallyDrop::new(value);
    std::ptr::read(&value as *const _ as *const To)
}
