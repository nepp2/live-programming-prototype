
#![allow(dead_code)]

#[cfg(test)]
#[macro_use] extern crate rusty_fork;

// #[macro_use] extern crate lazy_static;
// #[macro_use] extern crate maplit;

pub mod error;
pub mod lexer;
pub mod parser;
pub mod expr;
pub mod watcher;
pub mod structure;
pub mod typecheck2;
pub mod typecheck;
pub mod codegen;
pub mod jit;
pub mod repl;
pub mod c_interface;

#[cfg(test)]
mod test;

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::env;

use crate::jit::interpreter;
use crate::typecheck::Val;
use crate::error::Error;

pub fn print_result(r : Result<Val, Error>) -> String {
  match r {
    Ok(v) => format!("{:?}", v),
    Err(e) => format!( "{}", e),
  }
}

fn load_and_run(path : &str) {
  let path = PathBuf::from(path);
  let mut f = File::open(path).expect("file not found");
  let mut code = String::new();
  f.read_to_string(&mut code).unwrap();
  let mut i = interpreter();
  let result = i.run(&code);
  println!("{}", print_result(result));
}

fn main(){
  let args: Vec<String> = env::args().collect();
  let args: Vec<&str> = args.iter().map(|s| s.as_ref()).collect();
  match args[1..] {
    ["watch", f] => {
      let path = format!("code/{}.code", f);
      watcher::watch(path.as_ref())
    }
    ["watch"] => watcher::watch("code/scratchpad.code"),
    ["repl"] => repl::run_repl(),
    ["run", f] => load_and_run(f),
    _ => watcher::watch("code/scratchpad.code"),
  }
}