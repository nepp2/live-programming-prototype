
use std::fmt;
use std::rc::Rc;
use std::cell::RefCell;
use std::collections::HashSet;

use crate::error::{Error, TextLocation, error_raw };
use crate::c_interface::SStr;

/// An immutable, reference counted string
pub type RefStr = Rc<str>;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ListType {
  Normal = 0, Brace = 1, Bracket = 2, Uncontained = 3
}

#[repr(C, u8)]
#[derive(Debug)]
pub enum ExprContent {
  List(SArray<Expr>, ListType),
  Symbol(SStr),
  LiteralString(SStr),
  LiteralFloat(f64),
  LiteralInt(i64),
  LiteralBool(bool),
  LiteralUnit,
}

impl  Clone for ExprContent {
  fn clone(&self) -> Self {
    fn clone(s : SStr) -> SStr {
      let v : String = s.as_str().into();
      let s = SStr::from_str(s.as_str());
      std::mem::forget(v);
      s
    }
    use self::ExprContent::*;
    match self {
      Symbol(s) => Symbol(clone(*s)),
      List(l, pt) => List(l.clone(), *pt),
      LiteralString(s) => LiteralString(clone(*s)),
      LiteralFloat(f) => LiteralFloat(*f),
      LiteralInt(i) => LiteralInt(*i),
      LiteralBool(b) => LiteralBool(*b),
      LiteralUnit => LiteralUnit,
    }
  }
}

impl ExprContent {
  pub fn literal_string(s : String) -> ExprContent {
    let content = ExprContent::LiteralString(SStr::from_str(s.as_str()));
    std::mem::forget(s);
    content
  }
  pub fn symbol(s : String) -> ExprContent {
    let content = ExprContent::Symbol(SStr::from_str(s.as_str()));
    std::mem::forget(s);
    content
  }

  pub fn list(list : Vec<Expr>, list_type : ListType) -> ExprContent {
    ExprContent::List(SArray::new(list), list_type)
  }
}

#[repr(C)]
pub struct SArray<T> {
  pub data : *mut T,
  pub len : u64,
}

impl <T> SArray<T> {
  pub fn new(mut v : Vec<T>) -> SArray<T> {
    let a = SArray { len: v.len() as u64, data: v.as_mut_ptr() };
    std::mem::forget(v);
    a
  }

  pub fn empty_into_vec(&mut self) -> Vec<T> {
    // TODO: is this dodgy? this will frequently claim that the capacity 
    // is smaller than it really is.
    if self.len > 0 {
      let v = unsafe {
        Vec::from_raw_parts(self.data, self.len as usize, self.len as usize)
      };
      self.data = 0 as *mut T;
      self.len = 0;
      v
    }
    else {
      vec![]
    }
  }

  pub fn as_slice(&self) -> &[T] {
    unsafe { std::slice::from_raw_parts(self.data, self.len as usize) }
  }
}

impl <T : fmt::Debug> fmt::Debug for SArray<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{:?}", self.as_slice())
  }
}

impl <T> Drop for SArray<T> {
  fn drop(&mut self) {
    self.empty_into_vec();
  }
}

impl <T : Clone> Clone for SArray<T> {
  fn clone(&self) -> Self {
    let v = unsafe {
      Vec::from_raw_parts(self.data, self.len as usize, self.len as usize)
    };
    let a = SArray::new(v.clone());
    std::mem::forget(v);
    a
  }
}

#[repr(C)]
#[derive(Clone)]
pub struct Expr {
  pub loc : TextLocation,
  pub content : ExprContent,
}

impl Expr {
  pub fn new(content : ExprContent, loc : TextLocation) -> Expr {
    Expr { loc, content }
  }

  pub fn list(&self) -> Option<&[Expr]> {
    match &self.content {
      ExprContent::List(list, _) => Some(list.as_slice()),
      _ => None
    }
  }

  pub fn into_vec(mut self) -> Vec<Expr> {
    match &mut self.content {
      ExprContent::List(list, _) =>{
        list.empty_into_vec()
      }
      _ => vec![],
    }
  }

  pub fn try_head(&self) -> Option<&str> {
    self.list()?.first()?.try_symbol()
  }

  pub fn unwrap_head(&self) -> Result<&str, Error> {
    self.try_symbol().or_else(|| {
      self.list()?.first()?.try_symbol()
    })
    .ok_or_else(|| error_raw(self, "expected symbol"))
  }

  pub fn tail(&self) -> &[Expr] {
    if self.try_symbol().is_some() {
      &[]
    }
    else {
      &self.list().unwrap()[1..]
    }
  }

  pub fn try_symbol(&self) -> Option<&str> {
    match &self.content {
      ExprContent::Symbol(s) => Some(s.as_str()),
      _ => None,
    }
  }

  pub fn unwrap_symbol(&self) -> Result<&str, Error> {
    self.try_symbol()
      .ok_or_else(|| 
        error_raw(self, format!("expected a symbol, found {:?}", self.content)))
  }

  pub fn match_symbol<'e>(&'e self, s : &str) -> Option<&'e [Expr]> {
    if self.try_head() == Some(s) {
      return Some(self.tail());
    }
    None
  }
}

impl Drop for Expr {
  fn drop(&mut self) {
    // TODO: strings should be cleared somehow. this leaks memory badly.
  }
}

impl <'l> Into<TextLocation> for &'l Expr {
  fn into(self) -> TextLocation {
    self.loc
  }
}

impl fmt::Debug for Expr {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self)
  }
}

impl fmt::Display for Expr {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    fn display_inner(e: &Expr, f: &mut fmt::Formatter<'_>, indent : usize) -> fmt::Result {
      match &e.content {
        ExprContent::Symbol(s) => write!(f, "{}", s.as_str()),
        ExprContent::List(l, list_type) => {
          let l = l.as_slice();
          let (start_paren, end_paren) = match list_type {
            ListType::Normal | ListType::Uncontained => ("(", ")"),
            ListType::Brace => ("{", "}"),
            ListType::Bracket => ("[", "]"),
          };
          write!(f, "{}", start_paren)?;
          if *list_type == ListType::Brace {
            let indent = indent + 2;
            for c in l {
              writeln!(f)?;
              write!(f, "{:indent$}", "", indent=indent)?;
              display_inner(c, f, indent)?;
            }
          }
          else if l.len() > 0 {
            display_inner(&l[0], f, indent)?;
            for c in &l[1..] {
              write!(f, " ")?;
              display_inner(c, f, indent)?;
            }
          }
          write!(f, "{}", end_paren)?;
          return Ok(());
        }
        ExprContent::LiteralString(s) => write!(f, "{}", s.as_str()),
        ExprContent::LiteralFloat(v) => write!(f, "{}", v),
        ExprContent::LiteralInt(v) => write!(f, "{}", v),
        ExprContent::LiteralBool(v) => write!(f, "{}", v),
        ExprContent::LiteralUnit => write!(f, "()"),
      }
    }
    display_inner(&self, f, 0)
  }
}

/// This cache uses internal mutability to cache strings. It should be safe,
/// because the strings themselves are immutable.
#[derive(Default, Clone)]
pub struct StringCache {
  symbols : RefCell<HashSet<RefStr>>,
}

impl StringCache {
  pub fn new() -> StringCache {
    Default::default()
  }

  pub fn get<T : AsRef<str> + Into<RefStr>>(&self, s : T) -> RefStr {
    let mut symbols = self.symbols.borrow_mut();
    if let Some(symbol) = symbols.get(s.as_ref()) {
      symbol.clone()
    }
    else{
      let string : RefStr = s.into();
      symbols.insert(string.clone());
      string
    }
  }
}
