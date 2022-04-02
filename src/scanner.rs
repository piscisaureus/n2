//! Scans an input string (source file) character by character.

use crate::byte_string::*;
use std::ffi::OsStr;
use std::str::Utf8Error;
use std::string::FromUtf8Error;

#[derive(Debug)]
pub struct ParseError {
    msg: String,
    ofs: usize,
}

pub type ParseResult<T> = Result<T, ParseError>;

impl From<FromUtf8Error> for ParseError {
    fn from(err: FromUtf8Error) -> Self {
        Self::from(err.utf8_error())
    }
}

impl From<Utf8Error> for ParseError {
    fn from(err: Utf8Error) -> Self {
        Self {
            msg: err.to_string(),
            ofs: err.valid_up_to(),
        }
    }
}

pub struct Scanner<'a> {
    buf: &'a bstr,
    pub ofs: usize,
    pub line: usize,
}

impl<'a> Scanner<'a> {
    pub fn new(buf: &'a mut ByteString) -> Self {
        if !matches!(buf.last(), Some(0)) {
            buf.push(0);
        }
        Scanner {
            buf,
            ofs: 0,
            line: 1,
        }
    }

    pub fn slice(&self, start: usize, end: usize) -> &'a bstr {
        &self.buf[start..end]
    }
    pub fn peek(&self) -> u8 {
        self.buf[self.ofs]
    }
    pub fn next(&mut self) {
        if self.peek() == b'\n' {
            self.line += 1;
        }
        if self.ofs == self.buf.len() {
            panic!("scanned past end")
        }
        self.ofs += 1;
    }
    pub fn back(&mut self) {
        if self.ofs == 0 {
            panic!("back at start")
        }
        self.ofs -= 1;
        if self.peek() == b'\n' {
            self.line -= 1;
        }
    }
    pub fn read(&mut self) -> u8 {
        let c = self.peek();
        self.next();
        c
    }
    pub fn skip(&mut self, ch: u8) -> bool {
        if self.peek() == ch {
            self.next();
            return true;
        }
        false
    }

    pub fn skip_spaces(&mut self) {
        while self.skip(b' ') {}
    }

    pub fn expect(&mut self, ch: u8) -> ParseResult<()> {
        let r = self.read();
        if r != ch {
            self.back();
            return self.parse_error(format!("expected {:?}, got {:?}", ch as char, r as char));
        }
        Ok(())
    }

    pub fn parse_error<T, S: Into<String>>(&self, msg: S) -> ParseResult<T> {
        Err(ParseError {
            msg: msg.into(),
            ofs: self.ofs,
        })
    }

    pub fn format_parse_error(&self, filename: impl AsRef<OsStr>, err: ParseError) -> String {
        let filename = filename.as_ref();
        let mut ofs = 0;
        let lines = self.buf.split(|&c| c == b'\n');
        for (line_number, line) in lines.enumerate() {
            if ofs + line.len() >= err.ofs {
                let mut msg = "parse error: ".to_owned();
                msg.push_str(err.msg.as_str());
                msg.push('\n');

                let prefix = format!("{}:{}: ", filename.as_str_lossy(), line_number + 1);
                msg.push_str(&prefix);

                let context = String::from_utf8_lossy(line);
                let mut context = &*context;
                let mut col = err.ofs - ofs;
                if col > 40 {
                    // Trim beginning of line to fit it on screen.
                    msg.push_str("...");
                    context = &context[col - 20..];
                    col = 3 + 20;
                }
                if context.len() > 40 {
                    context = &context[0..40];
                    msg.push_str(context);
                    msg.push_str("...");
                } else {
                    msg.push_str(context);
                }
                msg.push('\n');

                msg.push_str(&" ".repeat(prefix.len() + col));
                msg.push_str("^\n");
                return msg;
            }
            ofs += line.len() + 1;
        }
        panic!("invalid offset when formatting error")
    }
}
