// external C interface for the compiler (so that the language can use it)

use crate::jit::Interpreter;
use crate::lexer;
use crate::expr::{RefStr, Expr};

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::collections::HashMap;
use std::path::Path;
use std::fmt;

use libloading::{Library, Symbol};

use std::{thread, time};

#[no_mangle]
pub extern fn create_interpreter() -> *mut Interpreter {
  Box::into_raw(Box::new(Interpreter::new()))
}

#[no_mangle]
pub extern fn drop_interpreter(i : *mut Interpreter) {
  unsafe { Box::from_raw(i) };
}

#[no_mangle]
pub extern fn lex_string(i : *mut Interpreter, code : *mut c_char) {
  let i = unsafe { &mut *i };
  let code = unsafe { CStr::from_ptr(code) };
  let _tokens =
    lexer::lex(code.to_str().unwrap(), &mut i.cache)
    .map_err(|mut es| es.remove(0)).unwrap();
  
}

/// A sized string that is compatible with the Cauldron's string representation
#[no_mangle]
#[derive(Copy, Clone)]
#[repr(C)]
pub struct SStr {
  pub ptr : *mut u8,
  pub length : u64,
}

impl SStr {
  pub fn from_str(s : &str) -> Self {
    let ptr = (s as *const str) as *mut u8;
    SStr { ptr, length: s.len() as u64 }
  }

  pub fn as_str(&self) -> &str {
    unsafe {
      let slice = std::slice::from_raw_parts(self.ptr, self.length as usize);
      std::str::from_utf8_unchecked(slice)
    }
  }
}

impl fmt::Debug for SStr {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.as_str())
  }
}

type VoidPtr = *mut ();

#[cfg(not(debug_assertions))]
static MODE : &'static str = "release";
#[cfg(debug_assertions)]
static MODE : &'static str = "debug";

#[cfg(not(test))]
static ROOT : &'static str = "";
#[cfg(test)]
static ROOT : &'static str = "../";

extern {
  pub fn malloc(size: usize) -> *mut u8;
}

#[no_mangle]
pub extern "C" fn print(s : SStr) {
  println!("{}", s.as_str());
}

/// defined for the test suite only
#[no_mangle]
pub extern "C" fn test_add(a : i64, b : i64) -> i64 {
  a + b
}

#[no_mangle]
pub extern "C" fn thread_sleep(millis : u64) {
  let t = time::Duration::from_millis(millis);
  thread::sleep(t);
}

#[no_mangle]
pub extern "C" fn load_library_c(lib_name : SStr) -> usize {
  let lib = lib_name.as_str();
  let deps_path = format!("{}target/{}/deps/{}.dll", ROOT, MODE, lib);
  let local_path = format!("{}.dll", lib);
  let paths = [deps_path.as_str(), local_path.as_str()];
  paths.iter().cloned().flat_map(load_library).nth(0).unwrap_or(0)
}

static mut SHARED_LIBRARIES : Option<HashMap<usize, (RefStr, Library)>> = None;
static mut SHARED_LIB_HANDLE_COUNTER : usize = 0;

/// TODO: This is not thread-safe!
pub fn load_library(path : &str) -> Option<usize> {
  let path = Path::new(path);
  let file_name = path.file_name().unwrap().to_str().unwrap();
  let r = Library::new(path);
  if r.is_err() {
    return None;
  }
  let lib = r.unwrap();
  unsafe {
    if SHARED_LIBRARIES.is_none() {
      SHARED_LIBRARIES = Some(HashMap::new());
    }
    SHARED_LIB_HANDLE_COUNTER += 1;
    let handle = SHARED_LIB_HANDLE_COUNTER;
    SHARED_LIBRARIES.as_mut().unwrap().insert(handle, (file_name.into(), lib));
    Some(handle)
  }
}

/// TODO: This is not thread-safe!
#[no_mangle]
pub extern "C" fn load_symbol(lib_handle : usize, symbol_name : SStr) -> usize {
  let s = CString::new(symbol_name.as_str()).unwrap();
  unsafe {
    if SHARED_LIBRARIES.is_none() {
      panic!();
    }
    let (_, lib) = SHARED_LIBRARIES.as_ref().unwrap().get(&lib_handle).unwrap();
    let symbol: Option<Symbol<*const ()>> =
      lib.get(s.as_bytes_with_nul()).ok();
    symbol.map(|sym| sym.into_raw().into_raw() as usize).unwrap_or(0)
  }
}

/// TODO: This is not thread-safe!
#[no_mangle]
pub extern "C" fn print_expr(e : &Expr) {
  println!("{}", e);
}

pub struct CLibraries {
  pub local_symbol_table : HashMap<RefStr, usize>,
  pub shared_libraries : HashMap<usize, (RefStr, Library)>,
  pub lib_handle_counter : usize,
}

impl CLibraries {
  pub fn new() -> CLibraries {
    let mut sym = HashMap::new();
    sym.insert("load_library".into(), (load_library_c as *const()) as usize);
    sym.insert("load_symbol".into(), (load_symbol as *const()) as usize);
    sym.insert("malloc".into(), (malloc as *const()) as usize);
    sym.insert("print".into(), (print as *const()) as usize);
    sym.insert("print_expr".into(), (print_expr as *const()) as usize);
    sym.insert("test_add".into(), (test_add as *const()) as usize);
    sym.insert("thread_sleep".into(), (thread_sleep as *const()) as usize);

    CLibraries {
      local_symbol_table: sym,
      shared_libraries: HashMap::new(),
      lib_handle_counter: 0,
    }
  }
}