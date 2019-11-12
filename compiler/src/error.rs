
use std::fmt;

/// Returns an error that isn't wrapped in Result::Err
pub fn error_raw<L : Into<TextLocation>, S : Into<ErrorContent>>(loc : L, message : S) -> Error {
  Error { message: message.into(), location: loc.into() }
}

/// Returns an error wrapped in Result::Err
pub fn error<T, L : Into<TextLocation>, S : Into<ErrorContent>>(loc : L, message : S) -> Result<T, Error> {
  Err(Error { message: message.into(), location: loc.into() })
}

/// Returns an error content wrapped
pub fn error_content<S : Into<ErrorContent>>(message : S) -> ErrorContent {
  message.into()
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMarker {
  pub line : usize,
  pub col : usize,
}

impl TextMarker {
  fn order(a : Self, b : Self) -> (Self, Self) {
    if a.line < b.line { (a, b) }
    else if b.line < a.line { (b, a) }
    else if a.col < b.col { (a, b) }
    else { (b, a) }
  }
}

impl fmt::Display for TextMarker {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "line: {}, column: {}", self.line, self.col)
  }
}

impl From<(usize, usize)> for TextMarker {
  fn from(v : (usize, usize)) -> TextMarker {
    TextMarker { line : v.0, col: v.1 }
  }
}

impl <'l> Into<TextLocation> for &'l TextLocation {
  fn into(self) -> TextLocation {
    *self
  }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextLocation {
  pub start : TextMarker,
  pub end : TextMarker,
}

impl fmt::Display for TextLocation {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "start: ({}), end: ({})", self.start, self.end)
  }
}

impl TextLocation {
  
  pub fn new<T : Into<TextMarker>>(start : T, end : T) -> TextLocation {
    TextLocation {
      start: start.into(),
      end: end.into(),
    }
  }
  
  pub fn zero() -> TextLocation {
    let z = TextMarker{ line: 0, col: 0 };
    TextLocation::new(z, z)
  }

  pub fn merge(self, x : Self) -> Self {
    let (start, _) = TextMarker::order(self.start, x.start);
    let (_, end) = TextMarker::order(self.end, x.end);
    TextLocation { start, end }
  }
}

impl <S : Into<String>> From<S> for ErrorContent {
  fn from(s : S) -> ErrorContent {
    let s : String = s.into();
    ErrorContent::Message(s)
  }
}

#[derive(Debug, PartialEq)]
pub enum ErrorContent {
  Message(String),
  InnerError(String, Box<Error>),
}

#[derive(Debug, PartialEq)]
pub struct Error {
  pub message : ErrorContent,
  pub location : TextLocation,
}

impl fmt::Display for Error {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    match &self.message {
      ErrorContent::Message(m) => {
        write!(f, "line: {}, column: {}, message: {}",
          self.location.start.line, self.location.start.col, m)
      },
      ErrorContent::InnerError(m, e) => {
        writeln!(f, "line: {}, column: {}, message: {}",
          self.location.start.line, self.location.start.col, m)?;
        writeln!(f, "  inner error:")?;
        write!(f, "    {}", e)
      },
    }
  }
}

