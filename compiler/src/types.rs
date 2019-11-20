
use std::fmt;
use itertools::Itertools;

use crate::error::TextLocation;
use crate::expr::UIDGenerator;
use crate::structure::{
  NodeId, Symbol, TypeKind, GlobalType,
};
use crate::arena::Arena;

use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct ModuleId(u64);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct FunctionId(u64);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct GenericId(u64);

impl From<u64> for ModuleId { fn from(v : u64) -> Self { ModuleId(v) } }
impl From<u64> for FunctionId { fn from(v : u64) -> Self { FunctionId(v) } }
impl From<u64> for GenericId { fn from(v : u64) -> Self { GenericId(v) } }

pub struct TypeInfo<'a> {
  pub id : ModuleId,
  pub type_defs : HashMap<&'a str, &'a TypeDefinition<'a>>,
  pub functions : HashMap<FunctionId, &'a FunctionDefinition<'a>>,
  pub globals : HashMap<&'a str, &'a GlobalDefinition<'a>>,
}

/// Primitive type
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PType {
  Void,
  F64, F32,
  I64, I32,
  U64, U32, U16, U8,
  Bool,
}

impl <'a> Into<Type<'a>> for PType {
  fn into(self) -> Type<'a> {
    Type::Prim(self)
  }
}

use PType::*;

#[derive(Clone, Copy, Debug)]
pub enum Type<'a> {
  /// Primitive type (e.g. int, float, bool, etc)
  Prim(PType),
  Generic(GenericId),
  Fun(&'a FunctionSignature<'a>),
  Def(&'a str),
  Array(&'a Type<'a>),
  Ptr(&'a Type<'a>),
}

#[derive(Clone, Debug)]
pub struct TypeDefinition<'a> {
  pub name : &'a str,
  pub fields : Vec<(Symbol, Type<'a>)>,
  pub kind : TypeKind,
  pub drop_function : Option<FunctionId>,
  pub clone_function : Option<FunctionId>,
  pub definition_location : TextLocation,
}

#[derive(Debug, Clone)]
pub enum FunctionImplementation<'a> {
  Normal{body: NodeId, name_for_codegen: &'a str, args : Vec<Symbol> },
  CFunction,
  Intrinsic,
}

#[derive(Debug, Clone)]
pub struct FunctionDefinition<'a> {
  pub id : FunctionId,
  pub module_id : ModuleId,
  pub name_in_code : &'a str,
  pub signature : &'a FunctionSignature<'a>,
  pub generics : Vec<GenericId>,
  pub implementation : FunctionImplementation<'a>,
  pub loc : TextLocation,
}

impl <'a> FunctionDefinition<'a> {
  pub fn codegen_name(&self) -> Option<&str> {
    match &self.implementation {
      FunctionImplementation::Normal{ name_for_codegen, .. } => Some(name_for_codegen),
      FunctionImplementation::CFunction => Some(&self.name_in_code),
      FunctionImplementation::Intrinsic => None,
    }
  }
}

#[derive(Debug, PartialEq, Clone)]
pub struct FunctionSignature<'a> {
  pub return_type : Type<'a>,
  pub args : Vec<Type<'a>>,
}

#[derive(Clone)]
pub struct GlobalDefinition<'a> {
  pub module_id : ModuleId,
  pub name : &'a str,
  pub type_tag : Type<'a>,
  pub global_type : GlobalType,
  pub loc : TextLocation,
}

impl <'a> PartialEq for Type<'a> {
  fn eq(&self, other: &Self) -> bool {
    use Type::*;
    match (*self, *other) {
      (Fun(a), Fun(b)) => a == b,
      (Def(a), Def(b)) => a == b,
      (Array(a), Array(b)) => a == b,
      (Ptr(a), Ptr(b)) => a == b,
      (Prim(a), Prim(b)) => a == b,
      (Generic(a), Generic(b)) => a == b,
      _ => false,
    }
  }
}

impl <'a> fmt::Display for Type<'a> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Type::Fun(sig) => {
        write!(f, "fun({}) => {}", 
          sig.args.iter().join(", "),
          sig.return_type)
      }
      Type::Def(name) => {
        write!(f, "{}", name)
      }
      Type::Array(t) => write!(f, "array({})", t),
      Type::Ptr(t) => write!(f, "ptr({})", t),
      Type::Generic(t) => write!(f, "@Generic"),
      Type::Prim(t) => write!(f, "{:?}", t),
    }
  }
}

impl <'a> Type<'a> {
  pub fn from_string(s : &str) -> Option<Type<'a>> {
    let pt = match s {
      "f64" => F64,
      "f32" => F32,
      "bool" => Bool,
      "i64" => I64,
      "u64" => U64,
      "i32" => I32,
      "u32" => U32,
      "u16" => U16,
      "u8" => U8,
      // "any" => Dynamic,
      "()" => Void,
      // "" => Dynamic,
      _ => return None,
    };
    Some(Type::Prim(pt))
  }

  pub fn float(&self) -> bool {
    match self { Type::Prim(F32) | Type::Prim(F64) => true, _ => false }
  }

  pub fn unsigned_int(&self) -> bool {
    match self { Type::Prim(U64) | Type::Prim(U32) | Type::Prim(U16) | Type::Prim(U8) => true, _ => false }
  }

  pub fn signed_int(&self) -> bool {
    match self { Type::Prim(I64) | Type::Prim(I32) => true, _ => false }
  }

  pub fn int(&self) -> bool {
    self.signed_int() || self.unsigned_int()
  }

  pub fn number(&self) -> bool {
    self.int() || self.float()
  }

  pub fn pointer(&self) -> bool {
    match self { Type::Ptr(_) | Type::Fun(_) => true, _ => false }
  }
}

pub enum FindFunctionResult<'a> {
  ConcreteFunction(FunctionId),
  GenericInstance(FunctionId, HashMap<GenericId, Type<'a>>),
}

impl <'a> TypeInfo<'a> {
  pub fn new(module_id : ModuleId) -> TypeInfo<'a> {
    TypeInfo{
      id: module_id,
      type_defs: HashMap::new(),
      functions: HashMap::new(),
      globals: HashMap::new(),
    }
  }

  pub fn find_global(&self, name : &str) -> Option<&GlobalDefinition<'a>> {
    self.globals.get(name).cloned()
  }

  pub fn find_type_def(&self, name : &str) -> Option<&TypeDefinition<'a>> {
    self.type_defs.get(name).cloned()
  }

  pub fn get_function(&self, fid : FunctionId) -> &FunctionDefinition<'a> {
    self.functions.get(&fid).unwrap()
  }

  pub fn find_function(&self, name : &str, args : &[Type<'a>]) -> Option<FindFunctionResult<'a>> {
    let r = self.functions.values().find(|def| {
      def.generics.is_empty() && def.name_in_code.as_ref() == name && {
        args == def.signature.args.as_slice()
      }
    });
    if let Some(def) = r {
      return Some(FindFunctionResult::ConcreteFunction(def.id));
    }
    let mut generics = HashMap::new();
    let r = self.functions.values().find(|def| {
      (!def.generics.is_empty()) && def.name_in_code.as_ref() == name && {
        let matched = generic_match_sig(&mut generics, args, def, def.signature);
        if !matched {
          generics.clear();
        }
        matched
      }
    });
    if let Some(def) = r {
      Some(FindFunctionResult::GenericInstance(def.id, generics))
    }
    else {
      None
    }
  }

  pub fn concrete_function(&'a self, arena : &'a Arena, gen : &mut UIDGenerator, r : FindFunctionResult<'a>) -> FunctionId {
    match r {
      FindFunctionResult::ConcreteFunction(fid) => fid,
      FindFunctionResult::GenericInstance(fid, generics) => {
        let mut sig = self.get_function(fid).signature.clone();
        for t in sig.args.iter_mut() {
          *t = self.generic_replace(arena, &generics, gen, *t);
        }
        sig.return_type = self.generic_replace(arena, &generics, gen, sig.return_type);
        let def = self.get_function(fid);
        let def = FunctionDefinition {
          id: FunctionId(gen.next()),
          module_id: self.id,
          name_in_code: def.name_in_code.clone(),
          signature: arena.alloc(sig),
          generics: vec![],
          implementation: def.implementation.clone(),
          loc: def.loc,
        };
        let fid = def.id;
        self.functions.insert(fid, arena.alloc(def));
        fid
      },
    }
  }
  
  fn generic_replace(&mut self, arena : &'a Arena, generics : &HashMap<GenericId, Type<'a>>, gen : &mut UIDGenerator, t : Type<'a>)
    -> Type
  {
    match t {
      Type::Ptr(t) => {
        let t = arena.alloc(self.generic_replace(arena, generics, gen, *t));
        Type::Ptr(t)
      }
      Type::Array(t) => {
        let t = arena.alloc(self.generic_replace(arena, generics, gen, *t));
        Type::Array(t)
      }
      Type::Generic(gid) => {
        *generics.get(&gid).unwrap()
      }
      _ => return t,
    }
  }
}

fn generic_match_sig<'a>(
  generics : &mut HashMap<GenericId, Type<'a>>, args : &[Type<'a>],
  def : &FunctionDefinition, sig : &FunctionSignature<'a>)
    -> bool
{
  args.len() == sig.args.len() && {
    for (t, gt) in args.iter().zip(sig.args.iter()) {
      if !generic_match(generics, *t, *gt) {
        return false;
      }
    }
    generics.len() == def.generics.len()
  }
}

fn generic_match<'a>(generics : &mut HashMap<GenericId, Type<'a>>, t : Type<'a>, gt : Type<'a>) -> bool {
  let (t, gt) = match (t, gt) {
    (Type::Ptr(t), Type::Ptr(gt)) => (t, gt),
    (Type::Array(t), Type::Array(gt)) => (t, gt),
    (t, Type::Generic(gid)) => {
      if let Some(bound_type) = generics.get(&gid) {
        return t == *bound_type;
      }
      else {
        generics.insert(gid, t);
        return true;
      }
    }
    _ => return t == gt,
  };
  generic_match(generics, *t, *gt)
}