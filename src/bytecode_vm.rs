
use error::{Error, TextLocation, error, error_raw, error_no_loc};
use parser::{Expr, Ast, ExprTag, ExprId};
use value::{Value, Struct, Array, StructVal, StructDef, RefStr, SymbolCache};
use typecheck::typecheck;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::cell::RefCell;
use std::usize;

#[derive(Debug)]
enum BytecodeInstruction {
  Push(Value),
  PushVar(usize),
  Pop,
  NewArray(usize),
  NewStruct(Rc<StructDef>),
  StructFieldInit(usize),
  PushStructField(RefStr),
  SetStructField(RefStr),
  ArrayIndex,
  SetArrayIndex,
  SetVar(usize),
  CallFunction(usize),
  JumpIfFalse(usize),
  Jump(usize),
  BinaryOperator(RefStr),
  UnaryOperator(RefStr),
}

use bytecode_vm::BytecodeInstruction as BC;

lazy_static! {
  static ref BYTECODE_OPERATORS : HashSet<&'static str> =
    vec!["+", "-", "*", "/", ">", "<", "<=", ">=",
    "==", "&&", "||", "-", "!"].into_iter().collect();
}

type FunctionBytecode = Vec<BytecodeInstruction>;

#[derive(Debug)]
struct FunctionInfo {
  name : RefStr,

  /// Number of arguments the function accepts
  arguments : Vec<RefStr>,

  /// Number of local variables which may be used. Arguments count towards this number.
  locals : usize,
}

#[derive(Debug)]
struct BytecodeProgram {
  bytecode : Vec<FunctionBytecode>,
  info : Vec<FunctionInfo>,
}

impl BytecodeProgram {
  fn new() -> BytecodeProgram {
    BytecodeProgram {
      bytecode: vec![],
      info: vec![],
    }
  }
}

struct FunctionDef {
  handle : usize,
  bytecode : FunctionBytecode,
  info : FunctionInfo,
}

struct VarScope {
  base_index : usize,
  vars : Vec<RefStr>,
}

struct LabelState {
  location : usize,
  references : Vec<usize>,
}

struct Environment<'l> {
  /// the "usize" represents the function handle
  functions : &'l mut HashMap<RefStr, FunctionDef>,

  structs : &'l mut HashMap<RefStr, Rc<StructDef>>,
  symbol_cache : &'l mut SymbolCache,

  /// function name
  function_name : RefStr,

  /// the arguments that this function receives
  arguments : Vec<RefStr>,

  /// stores offset of each local variable in the stack frame
  locals : Vec<VarScope>,

  /// keeps track of labels
  labels : HashMap<RefStr, LabelState>,

  /// the maximum number of locals visible in any scope of the function
  max_locals : usize,

  instructions : Vec<BytecodeInstruction>,

  /// indicates how many nested loops we are inside in the currently-executing function
  loop_break_labels : Vec<RefStr>,
}

impl <'l> Environment<'l> {
  fn new(
    function_name : RefStr,
    arguments : Vec<RefStr>,
    functions : &'l mut HashMap<RefStr, FunctionDef>,
    structs : &'l mut HashMap<RefStr, Rc<StructDef>>,
    symbol_cache : &'l mut SymbolCache,
  ) -> Environment<'l> {
    let vs = VarScope { base_index: 0, vars: arguments.clone() };
    let locals = vec![vs];
    Environment{
      function_name, arguments, functions,
      structs, symbol_cache, locals,
      labels: HashMap::new(),
      max_locals: 0,
      instructions: vec!(),
      loop_break_labels: vec!(),
    }
  }

  fn complete(mut self) {
    // Fix the jump locations
    for (_, label) in self.labels {
      for r in label.references {
        let instr = match &self.instructions[r] {
          BC::Jump(_) => BC::Jump(label.location),
          BC::JumpIfFalse(_) => BC::JumpIfFalse(label.location),
          i => panic!("expected label and found {:?}", i),
        };
        self.instructions[r] = instr;
      }
    }
    let function = FunctionDef {
      handle: self.functions.len(),
      bytecode: self.instructions,
      info: FunctionInfo { name: self.function_name.clone(), arguments: self.arguments, locals: self.max_locals },
    };
    self.functions.insert(self.function_name, function);
  }

  fn emit(&mut self, instruction : BytecodeInstruction, do_emit : bool) {
    if do_emit {
      self.instructions.push(instruction);
    }
  }

  fn emit_always(&mut self, instruction : BytecodeInstruction) {
    self.instructions.push(instruction);
  }

  fn emit_label(&mut self, label : RefStr) {
    let location = self.instructions.len();
    let current_location = &mut self.labels.get_mut(&label).unwrap().location;
    if *current_location != usize::MAX {
      panic!("label used twice in compiler");
    }
    *current_location = location;
  }

  fn emit_jump(&mut self, label : &str) {
    let location = self.instructions.len();
    self.labels.get_mut(label).unwrap().references.push(location);
    self.emit_always(BC::Jump(usize::MAX))
  }

  fn emit_jump_if_false(&mut self, label : &str) {
    let location = self.instructions.len();
    self.labels.get_mut(label).unwrap().references.push(location);
    self.emit_always(BC::JumpIfFalse(usize::MAX))
  }

  fn label(&mut self, s : &str) -> RefStr {
    let mut i = 0;
    let mut label_string;
    // TODO: this is not very efficient, and should maybe be fixed
    loop {
      label_string = format!("{}_{}",s, i);
      if !self.labels.contains_key(label_string.as_str()) {
        break;
      }
      i += 1;
    }
    let label = self.symbol_cache.symbol(label_string);
    self.labels.insert(label.clone(), LabelState { location: usize::MAX, references: vec!() });
    label
  }

  fn count_locals(&self) -> usize {
    if self.locals.len() == 0 {
      0
    }
    else {
      let vs = &self.locals[self.locals.len()-1];
      vs.base_index + vs.vars.len()
    }
  }

  fn find_var_offset(&self, v : &str, loc : &TextLocation) -> Result<usize, Error> {
    for vs in self.locals.iter().rev() {
      for i in (0..vs.vars.len()).rev() {
        if vs.vars[i].as_ref() == v {
          return Ok(vs.base_index + i);
        }
      }
    }
    error(*loc, format!("no variable called '{}' found in scope", v))
  }
}

fn compile_function_call(function_name_id: ExprId, args: &[ExprId], ast : &Ast, env : &mut Environment, push_answer : bool)
  -> Result<(), Error>
{
  let expr = ast.expr(function_name_id);
  let function_name = expr.symbol_unwrap()?;
  for i in 0..args.len() {
    compile(args[i], ast, env, true)?;
  }
  let handle = env.functions.get(function_name)
    .ok_or_else(
      ||error_raw(expr, format!("Found no function called '{}'", function_name)))?
    .handle;
  env.emit_always(BC::CallFunction(handle));
  if !push_answer {
    env.emit_always(BC::Pop);
  }
  Ok(())
}

fn compile_tree(id : ExprId, ast : &Ast, env : &mut Environment, push_answer : bool) -> Result<(), Error> {

  fn does_not_push(expr : &Expr, push_answer : bool) -> Result<(), Error> {
    if push_answer {
      let instr = expr.tree_symbol_unwrap()?.as_ref();
      error(expr, format!("instruction '{}' is void, where a result is expected", instr))
    }
    else {
      Ok(())
    }
  }

  let expr = ast.expr(id);
  let instr = expr.tree_symbol_unwrap()?.as_ref();
  let children = expr.children.as_slice();
  match (instr, children) {
    ("call", exprs) => {
      let symbol = ast.expr(exprs[0]).symbol_unwrap()?;
      let params = &exprs[1..];
      if BYTECODE_OPERATORS.contains(symbol.as_ref()) {
        match params {
          [a, b] => {
            compile(*a, ast, env, push_answer)?;
            compile(*b, ast, env, push_answer)?;
            env.emit(BC::BinaryOperator(symbol.clone()), push_answer);
          }
          [v] => {
            compile(*v, ast, env, push_answer)?;
            env.emit(BC::UnaryOperator(symbol.clone()), push_answer);
          }
          _ => {
            return error(expr, format!("wrong number of arguments for operator"));
          }
        }
      }
      else {
        compile_function_call(exprs[0], params, ast, env, push_answer)?;
      }
    }
    ("block", exprs) => {
      let v = VarScope { base_index: env.count_locals(), vars: vec!() };
      env.locals.push(v);
      let num_exprs = exprs.len();
      if num_exprs > 1 {
        for i in 0..(num_exprs-1) {
          compile(exprs[i], ast, env, false)?;
        }
      }
      compile(exprs[num_exprs-1], ast, env, push_answer)?;
      let new_local_count = env.count_locals();
      if new_local_count > env.max_locals {
        env.max_locals = new_local_count;
      }
      env.locals.pop();
    }
    ("let", exprs) => {
      does_not_push(expr, push_answer)?;
      let name = ast.expr(exprs[0]).symbol_unwrap()?;
      compile(exprs[1], ast, env, true)?;
      let offset = env.count_locals();
      env.locals.last_mut().unwrap().vars.push(name.clone());
      env.emit_always(BC::SetVar(offset));
    }
    ("=", [_, assign_expr, value_expr]) => {
      let assign_expr = ast.expr(*assign_expr);
      match &assign_expr.tag {
        ExprTag::Symbol(var_symbol) => {
          does_not_push(expr, push_answer)?;
          compile(*value_expr, ast, env, true)?; // emit value
          let offset = env.find_var_offset(&var_symbol, &assign_expr.loc)?;
          env.emit_always(BC::SetVar(offset));
          return Ok(());
        }
        ExprTag::Tree(symbol) => {
          does_not_push(expr, push_answer)?;
          match (symbol.as_ref(), assign_expr.children.as_slice()) {
            ("index", [array_expr, index_expr]) => {
              compile(*array_expr, ast, env, true)?;
              compile(*index_expr, ast, env, true)?;
              compile(*value_expr, ast, env, true)?;
              env.emit_always(BC::SetArrayIndex);
              return Ok(());
            }
            (".", [struct_expr, field_expr]) => {
              compile(*struct_expr, ast, env, true)?;
              compile(*value_expr, ast, env, true)?;
              let field_name = ast.expr(*field_expr).symbol_unwrap()?;
              env.emit_always(BC::SetStructField(field_name.clone()));
              return Ok(());
            }
            _ => (),
          }
        }
        _ => (),
      }
      return error(assign_expr, format!("can't assign to {:?}", assign_expr));
    }
    ("if", exprs) => {
      let arg_count = exprs.len();
      if arg_count < 2 || arg_count > 3 {
        return error(expr, "malformed if expression");
      }
      let false_label = env.label("if_false_label");
      if arg_count == 3 {
        // has else branch
        let else_end_label = env.label("else_end_label");
        compile(exprs[0], ast, env, true)?;
        env.emit_jump_if_false(&false_label);
        compile(exprs[1], ast, env, push_answer)?;
        env.emit_jump(&else_end_label);
        env.emit_label(false_label);
        compile(exprs[2], ast, env, push_answer)?;
        env.emit_label(else_end_label);
      }
      else {
        // has no else branch
        does_not_push(expr, push_answer)?;
        compile(exprs[0], ast, env, true)?;
        env.emit_jump_if_false(&false_label);
        compile(exprs[1], ast, env, false)?;
        env.emit_label(false_label);
      }
    }
    ("struct_define", exprs) => {
      if exprs.len() < 1 {
        return error(expr, "malformed struct definition");
      }
      let name_id = exprs[0];
      let name = ast.expr(name_id).symbol_to_refstr()?;
      if env.structs.contains_key(&name) {
        return error(ast.expr(name_id), format!("A struct called {} has already been defined.", name));
      }
      // TODO: check for duplicates?
      let field_exprs = &exprs[1..];
      let mut fields = vec![];
      for i in (0..(field_exprs.len()-1)).step_by(2) {
        fields.push(ast.expr(field_exprs[i]).symbol_to_refstr()?);
      }
      let def = Rc::new(StructDef { name: name.clone(), fields });
      env.structs.insert(name, def);
    }
    ("struct_instantiate", exprs) => {
      if exprs.len() < 1 || exprs.len() % 2 == 0 {
        return error(expr, format!("malformed struct instantiation {:?}", exprs));
      }
      let name_id = exprs[0];
      let name = ast.expr(name_id).symbol_to_refstr()?;
      let def =
        env.structs.get(name.as_ref())
        .ok_or_else(|| error_raw(*ast.loc(name_id), format!("struct {} does not exist", name)))?.clone();
      env.emit(BC::NewStruct(def.clone()), push_answer);
      {
        let mut field_index_map =
          def.fields.iter().enumerate()
          .map(|(i, s)| (s.as_ref(), i)).collect::<HashMap<&str, usize>>();
        for i in (1..exprs.len()).step_by(2) {
          let field_name = ast.expr(exprs[i]).symbol_to_refstr()?;
          compile(exprs[i+1], ast, env, push_answer)?;
          let index = field_index_map.remove(field_name.as_ref())
            .ok_or_else(|| error_raw(*ast.loc(exprs[i]), format!("field {} does not exist", name)))?;
          env.emit(BC::StructFieldInit(index), push_answer);
        }
        if field_index_map.len() > 0 {
          return error(expr, "Some fields not initialised");
        }
      }
    }
    (".", [_, expr, field_name]) => {
      compile(*expr, ast, env, push_answer)?;
      let name = ast.expr(*field_name).symbol_unwrap()?;
      env.emit(BC::PushStructField(name.clone()), push_answer);
    }
    ("while", exprs) => {
      does_not_push(expr, push_answer)?;
      if exprs.len() != 2 {
        return error(expr, "malformed while block");
      }
      let condition_label = env.label("loop_condition");
      let end_label = env.label("loop_end");
      env.loop_break_labels.push(end_label.clone());
      env.emit_label(condition_label.clone());
      compile(exprs[0], ast, env, true)?; // emit condition
      env.emit_jump_if_false(&end_label); // exit loop if condition fails
      compile(exprs[1], ast, env, false)?; // emit loop body
      env.emit_jump(&condition_label); // jump back to the condition
      env.emit_label(end_label);
      env.loop_break_labels.pop();
    }
    ("fun", exprs) => {
      let name = ast.expr(exprs[0]).symbol_unwrap()?;
      let args_exprs = ast.children(exprs[1])?;
      let function_body = exprs[2];
      let mut params = vec![];
      for i in (0..(args_exprs.len()-1)).step_by(2) {
        let e = ast.expr(args_exprs[i]);
        params.push(e.symbol_to_refstr()?);
      }
      let mut new_env = Environment::new(name.clone(), params, &mut env.functions, &mut env.structs, &mut env.symbol_cache);
      compile(function_body, ast, &mut new_env, true)?;
      new_env.complete();
    }
    ("literal_array", exprs) => {
      for e in exprs {
        compile(*e, ast, env, push_answer)?;
      }
      env.emit(BC::NewArray(exprs.len()), push_answer);
    }
    ("index", exprs) => {
      if let [array_expr, index_expr] = exprs {
        compile(*array_expr, ast, env, push_answer)?;
        compile(*index_expr, ast, env, push_answer)?;
        env.emit(BC::ArrayIndex, push_answer);
      }
      else {
        return error(expr, format!("index instruction expected 2 arguments. Found {}.", exprs.len()));
      }
    }
    _ => {
      return error(expr, format!("instruction '{}' with {} args is not supported by the interpreter.", instr, children.len()));
    }
  }
  Ok(())
}


fn compile(e : ExprId, ast : &Ast, env : &mut Environment, push_answer : bool) -> Result<(), Error> {
  match ast.tag(e) {
    ExprTag::Tree(_) => {
      compile_tree(e, ast, env, push_answer)?;
    }
    ExprTag::Symbol(s) => {
      if s.as_ref() == "break" {
        if let Some(l) = env.loop_break_labels.last().map(|s| s.clone()) {
          env.emit_jump(l.as_ref());
        }
        else {
          return error(*ast.loc(e), "can't break outside a loop");
        }
      }
      else {
        let offset = env.find_var_offset(&s, &ast.loc(e))?;
        env.emit(BC::PushVar(offset), push_answer);
      }
    }
    ExprTag::LiteralFloat(f) => {
      let v = Value::Float(*f);
      env.emit(BC::Push(v), push_answer);
    }
    ExprTag::LiteralBool(b) => {
      let v = Value::Bool(*b);
      env.emit(BC::Push(v), push_answer);
    }
  }
  Ok(())
}

fn compile_bytecode(ast : &Ast, entry_function_name : RefStr, symbol_cache : &mut SymbolCache) -> Result<BytecodeProgram, Error> {
  let mut functions = HashMap::new();
  let mut structs = HashMap::new();
  {
    let mut env = Environment::new(entry_function_name, vec![], &mut functions, &mut structs, symbol_cache);
    compile(ast.root_id, ast, &mut env, true)?;
    env.complete();
  }
  let mut bp = BytecodeProgram::new();
  let mut defs = functions.into_iter().map(|x| x.1).collect::<Vec<FunctionDef>>();
  defs.sort_unstable_by_key(|d| d.handle);
  for def in defs {
    bp.bytecode.push(def.bytecode);
    bp.info.push(def.info);
  }
  Ok(bp)
}

fn to_f(v : Value) -> Result<f32, Error> {
  match v {
    Value::Float(f) => Ok(f),
    x => Err(error_no_loc(format!("Expected float, found {:?}.", x)))
  }
}
fn to_b(v : Value) -> Result<bool, Error> {
  match v {
    Value::Bool(b) => Ok(b),
    x => Err(error_no_loc(format!("Expected boolean, found {:?}.", x)))
  }
}
fn to_array(v : Value) -> Result<Array, Error> {
  match v {
    Value::Array(a) => Ok(a),
    x => Err(error_no_loc(format!("Expected array, found {:?}.", x)))
  }
}
fn to_struct(v : &Value) -> Result<&StructVal, Error> {
  match v {
    Value::Struct(s) => Ok(s),
    x => Err(error_no_loc(format!("Expected struct, found {:?}.", x)))
  }
}
fn struct_field_index(def : &StructDef, field_name : &str) -> Result<usize, Error> {
  def.fields.iter().position(|s| s.as_ref() == field_name)
  .ok_or_else(||error_no_loc(format!("field {} does not exist on struct '{:?}'.", field_name, def)))
}

fn array_index(array : &Vec<Value>, index : f32) -> Result<usize, Error> {
  let i = index as usize;
  if index >= 0.0 && i < array.len() {
    Ok(i)
  }
  else {
    Err(error_no_loc(format!("Index out of bounds error. Array of {} elements given index {}.", array.len(), index)))
  }
}

struct Call {
  function_handle : usize,
  var_base : usize,
  program_counter : usize,
}

fn new_function_call(function_handle : usize, stack : &mut Vec<Value>, info : &Vec<FunctionInfo>) -> Call{
  let info = &info[function_handle];
  let args = info.arguments.len();
  for _ in 0..(info.locals - args) {
    stack.push(Value::Unit);
  }
  Call {
    function_handle,
    var_base: stack.len() - info.locals,
    program_counter: 0,
  }
}

fn interpret_bytecode(program : &BytecodeProgram, entry_function : usize) -> Result<Value, Error> {
  let mut stack : Vec<Value> = vec![];
  let mut callstack : Vec<Call> = vec![];
  let mut c = new_function_call(entry_function, &mut stack, &program.info);
  loop {
    let instructions = &program.bytecode[c.function_handle];
    loop {
      if c.program_counter >= instructions.len() {
        // Return (lol)
        let return_value = stack.pop().unwrap();
        if c.var_base == 0 {
          return Ok(return_value);
        }
        stack.truncate(c.var_base);
        stack.push(return_value);
        c = callstack.pop().unwrap();
        c.program_counter += 1;
        break;
      }
      // println!("stack: {:?}", stack);
      // println!("PC: {}, instruction: {:?}", c.program_counter, &instructions[c.program_counter]);    
      match &instructions[c.program_counter] {
        BC::Push(value) => {
          stack.push(value.clone());
        }
        BC::Pop => {
          stack.pop();
        }
        BC::PushVar(var_slot) => {
          let v = stack[c.var_base + *var_slot].clone();
          stack.push(v);
        }
        BC::NewArray(elements) => {
          let mut a = vec!(Value::Unit ; *elements);
          for i in (0..*elements).rev() {
            a[i] = stack.pop().unwrap();
          }
          let array = Rc::new(RefCell::new(a));
          stack.push(Value::Array(array));
        }
        BC::ArrayIndex => {
          let float_index = to_f(stack.pop().unwrap())?;
          let a = to_array(stack.pop().unwrap())?;
          let a = a.borrow();
          let i = array_index(&a, float_index)?;
          stack.push(a[i].clone());
        }
        BC::SetArrayIndex => {
          let v = stack.pop().unwrap();
          let f_index = to_f(stack.pop().unwrap())?;
          let a = to_array(stack.pop().unwrap())?;
          let mut array = a.borrow_mut();
          let i = array_index(&array, f_index)?;
          array[i] = v;
        }
        BC::NewStruct(def) => {
          let fields = vec![Value::Unit ; def.fields.len()];
          let s = Rc::new(RefCell::new(Struct { def: def.clone(), fields }));
          stack.push(Value::Struct(s));
        }
        BC::StructFieldInit(index) => {
          let v = stack.pop().unwrap();
          let s = to_struct(stack.last().unwrap())?;
          s.borrow_mut().fields[*index] = v;
        }
        BC::PushStructField(name) => {
          let s = stack.pop().unwrap();
          let s = to_struct(&s)?;
          let index = struct_field_index(&s.borrow().def, name)?;
          let v = s.borrow().fields[index].clone();
          stack.push(v);
        }
        BC::SetStructField(name) => {
          let v = stack.pop().unwrap();
          let s = stack.pop().unwrap();
          let s = to_struct(&s)?;
          let index = struct_field_index(&s.borrow().def, name)?;
          s.borrow_mut().fields[index] = v;
        }
        BC::SetVar(var_slot) => {
          let v = stack.pop().unwrap();
          stack[c.var_base + *var_slot] = v;
        }
        BC::CallFunction(handle) => {
          callstack.push(c);
          c = new_function_call(*handle, &mut stack, &program.info);
          break;
        }
        BC::JumpIfFalse(location) => {
          if !to_b(stack.pop().unwrap())? {
            c.program_counter = *location;
            continue;
          }
        }
        BC::Jump(location) => {
          c.program_counter = *location;
          continue;
        }
        BC::BinaryOperator(operator) => {
          let b = stack.pop().unwrap();
          let a = stack.pop().unwrap();
          let v = match operator.as_ref() {
            "+" => Value::Float(to_f(a)? + to_f(b)?),
            "-" => Value::Float(to_f(a)? - to_f(b)?),
            "*" => Value::Float(to_f(a)? * to_f(b)?),
            "/" => Value::Float(to_f(a)? / to_f(b)?),
            ">" => Value::Bool(to_f(a)? > to_f(b)?),
            "<" => Value::Bool(to_f(a)? < to_f(b)?),
            "<=" => Value::Bool(to_f(a)? <= to_f(b)?),
            ">=" => Value::Bool(to_f(a)? >= to_f(b)?),
            "==" => Value::Bool(a == b),
            "&&" => Value::Bool(to_b(a)? && to_b(b)?),
            "||" => Value::Bool(to_b(a)? || to_b(b)?),
            op => return Err(error_no_loc(format!("unsupported binary operator {}", op))),
          };
          stack.push(v);
        }
        BC::UnaryOperator(operator) => {
          let a = stack.pop().unwrap();
          let v = match operator.as_ref() {
            "-" => Value::Float(-to_f(a)?),
            "!" => Value::Bool(!to_b(a)?),
            op => return Err(error_no_loc(format!("unsupported unary operator {}", op))),
          };
          stack.push(v);
        }
        // TODO remove: i => return error("instruction '{:?}' not yet implemented.", i)),
      }
      c.program_counter += 1;
    }
  }
}

pub fn interpret(ast : &Ast) -> Result<Value, Error> {
  let mut symbol_cache = SymbolCache::new();
  let entry_function_name = symbol_cache.symbol("main");
  let program = compile_bytecode(ast, entry_function_name.clone(), &mut symbol_cache)?;
  for (info, bytecode) in program.info.iter().zip(program.bytecode.iter()) {
    println!("--------------------------------");
    println!("Function '{}':", info.name);
    for (i, instr) in bytecode.iter().enumerate() {
      println!("{}:   {:?}", i, instr);
    }
    println!();
    println!("Max local variables: {}", info.locals);
    println!();
  }
  let entry_function_handle = program.info.iter().position(|i| i.name == entry_function_name).unwrap();
  interpret_bytecode(&program, entry_function_handle)
}
