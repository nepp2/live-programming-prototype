
/*

Modified from code released under the license below:

######################################################
Copyright (c) 2014 Jauhien Piatlicki

Permission is hereby granted, free of charge, to any person obtaining
a copy of this software and associated documentation files (the
"Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to
the following conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

[Except as contained in this notice, the name of <copyright holders>
shall not be used in advertising or otherwise to promote the sale, use
or other dealings in this Software without prior written authorization
from Jauhien Piatlicki.]
######################################################

*/

// TODO: Carlos says I should have more comments than the occasional TODO

use rustyline::Editor;

use crate::error::{Error, error, error_raw, ErrorContent};
use crate::value::{SymbolTable, display_expr, RefStr, Expr};
use crate::lexer;
use crate::parser;
use crate::parser::ReplParseResult::{Complete, Incomplete};
use crate::typecheck::{
  AstNode, Content, Type, Val, StructDefinition, FunctionDefinition, TypeChecker, VarScope};

use dlltest;

use std::rc::Rc;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;

use llvm_sys::support::LLVMLoadLibraryPermanently;

use inkwell::AddressSpace;
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::{Context};
use inkwell::module::{Module, Linkage};
use inkwell::passes::PassManager;
use inkwell::types::{BasicTypeEnum, BasicType, StructType, PointerType, FunctionType, AnyTypeEnum};
use inkwell::values::{
  BasicValueEnum, BasicValue, FloatValue, IntValue, FunctionValue, PointerValue };
use inkwell::{OptimizationLevel, FloatPredicate, IntPredicate};
use inkwell::execution_engine::ExecutionEngine;

fn dump_module(module : &Module) {
  println!("{}", module.print_to_string().to_string())
}

macro_rules! codegen_type {
  (FloatValue, $e:ident, $jit:ident) => { $jit.codegen_float($e) };
  (IntValue, $e:ident, $jit:ident) => { $jit.codegen_int($e) };
}

macro_rules! binary_op {
  ($op_name:ident, $type_name:ident, $a:ident, $b:ident, $jit:ident) => {
    {
      let a = codegen_type!($type_name, $a, $jit)?;
      let b = codegen_type!($type_name, $b, $jit)?;
      let fv = ($jit).builder.$op_name(a, b, "op_result");
      reg(fv.into())
    }
  }
}

macro_rules! unary_op {
  ($op_name:ident, $type_name:ident, $a:ident, $jit:ident) => {
    {
      let a = codegen_type!($type_name, $a, $jit)?;
      let fv = ($jit).builder.$op_name(a, "op_result");
      reg(fv.into())
    }
  }
}

macro_rules! compare_op {
  ($op_name:ident, $pred:expr, $type_name:ident, $a:ident, $b:ident, $jit:ident) => {
    {
      let a = codegen_type!($type_name, $a, $jit)?;
      let b = codegen_type!($type_name, $b, $jit)?;
      let fv = ($jit).builder.$op_name($pred, a, b, "cpm_result");
      reg(fv.into())
    }
  }
}

/// Indicates whether a value is a pointer to the stack,
/// or stored directly in a register.
#[derive(PartialEq)]
enum Storage {
  Register,
  Stack,
}

#[derive(PartialEq)]
struct JitVal {
  storage : Storage,
  value : BasicValueEnum,
}

fn reg(value : BasicValueEnum) -> JitVal {
  JitVal { storage: Storage::Register, value }
}

fn stack(ptr : PointerValue) -> JitVal {
  JitVal { storage: Storage::Stack, value: ptr.as_basic_value_enum() }
}

struct LoopLabels {
  condition : BasicBlock,
  exit : BasicBlock,
}

/*
impl From<Option<BasicValueEnum>> for JitVal {
  fn from(v : Option<BasicValueEnum>) -> JitVal {
    match v {
      Some(v) => JitVal::Register(v),
      None => JitVal::Void,
    }
  }
}
*/

fn const_null(t : BasicTypeEnum) -> BasicValueEnum {
  use BasicTypeEnum::*;
  match t {
    FloatType(t) => t.const_null().into(),
    IntType(t) => t.const_null().into(),
    ArrayType(t) => t.const_null().into(),
    PointerType(t) => t.const_null().into(),
    StructType(t) => t.const_null().into(),
    VectorType(t) => t.const_null().into(),
  }
}

fn basic_type(t : AnyTypeEnum) -> Option<BasicTypeEnum> {
  use AnyTypeEnum::*;
  let bt = match t {
    FloatType(t) => t.as_basic_type_enum(),
    IntType(t) => t.as_basic_type_enum(),
    ArrayType(t) => t.as_basic_type_enum(),
    PointerType(t) => t.as_basic_type_enum(),
    StructType(t) => t.as_basic_type_enum(),
    VectorType(t) => t.as_basic_type_enum(),
    VoidType(_t) => return None,
    FunctionType(_t) => return None,
  };
  Some(bt)
}

#[derive(Copy, Clone)]
enum ShortCircuitOp { And, Or }

pub struct Jit<'l> {
  context: &'l mut Context,
  module : &'l mut Module,
  builder: Builder,
  functions: &'l mut HashMap<RefStr, FunctionValue>,
  function_defs: &'l HashMap<RefStr, Rc<FunctionDefinition>>,
  global_var_types: &'l HashMap<RefStr, Type>,
  variables: HashMap<RefStr, PointerValue>,
  struct_types: HashMap<RefStr, StructType>,

  /// A stack of values indicating the entry and exit labels for each loop
  loop_labels: Vec<LoopLabels>,

  pm : &'l mut PassManager,
}

impl <'l> Jit<'l> {

  pub fn new(
    context: &'l mut Context,
    module : &'l mut Module,
    functions: &'l mut HashMap<RefStr, FunctionValue>,
    function_defs: &'l HashMap<RefStr, Rc<FunctionDefinition>>,
    global_var_types: &'l HashMap<RefStr, Type>,
    pm : &'l mut PassManager,
  )
      -> Jit<'l>
  {
    let builder = context.create_builder();
    let variables = HashMap::new();
    Jit {
      context, module, builder,
      functions, function_defs,
      global_var_types, variables,
      struct_types: HashMap::new(),
      loop_labels: vec!(),
      pm,
    }
  }

  pub fn child_function_jit<'lc>(&'lc mut self, global_var_types: &'lc HashMap<RefStr, Type>)
    -> Jit<'lc>
  {
    Jit::new(self.context, self.module, self.functions, self.function_defs, global_var_types, self.pm)
  }

  fn create_entry_block_alloca(&self, t : BasicTypeEnum, name : &str) -> PointerValue {
    let current_block = self.builder.get_insert_block().unwrap();
    let function = current_block.get_parent().unwrap();
    let entry = function.get_entry_basic_block().unwrap();
    match entry.get_first_instruction() {
      Some(fi) => self.builder.position_before(&fi),
      None => self.builder.position_at_end(&entry),
    }
    let pointer = self.builder.build_alloca(t, name);
    self.builder.position_at_end(&current_block);
    pointer
  }

  fn init_variable(&mut self, name : RefStr, var_scope : VarScope, value : BasicValueEnum)
    -> Result<(), ErrorContent>
  {
    if self.variables.contains_key(&name) {
      return Err("variable with this name already defined".into());
    }
    let pointer = match var_scope {
      VarScope::Global => {
        let gv = self.module.add_global(value.get_type(), Some(AddressSpace::Global), &name);
        let null = const_null(value.get_type());
        gv.set_initializer(&null);
        gv.set_constant(false);
        gv.as_pointer_value()
      }
      VarScope::Local => {
        self.create_entry_block_alloca(value.get_type(), &name)
      }
    };
    self.builder.build_store(pointer, value);
    self.variables.insert(name, pointer);
    Ok(())
  }

  fn codegen_value(&mut self, n : &AstNode) -> Result<BasicValueEnum, Error> {
    Ok(self.codegen_expression_to_register(n)?.unwrap())
  }

  fn codegen_float(&mut self, n : &AstNode) -> Result<FloatValue, Error> {
    let v = self.codegen_value(n)?;
    match v {
      BasicValueEnum::FloatValue(f) => Ok(f),
      t => error(n.loc, format!("Expected float, found {:?}", t)),
    }
  }

  fn codegen_int(&mut self, n : &AstNode) -> Result<IntValue, Error> {
    let v = self.codegen_value(n)?;
    match v {
      BasicValueEnum::IntValue(i) => Ok(i),
      t => error(n.loc, format!("Expected int, found {:?}", t)),
    }
  }

  fn codegen_expression_to_register(&mut self, ast : &AstNode) -> Result<Option<BasicValueEnum>, Error> {
    if let Some(v) = self.codegen_expression(ast)? {
      let reg_val = match v.storage {
        Storage::Stack => {
          let ptr = *v.value.as_pointer_value();
          self.builder.build_load(ptr, "assignment_value")
        }
        Storage::Register => {
          v.value
        }
      };
      Ok(Some(reg_val))
    }
    else {
      Ok(None)
    }
  }

  fn codegen_short_circuit_op(&mut self, a : &AstNode, b : &AstNode, op : ShortCircuitOp)
    -> Result<JitVal, Error>
  {
    use ShortCircuitOp::*;
    let short_circuit_outcome = match op {
      And => self.context.bool_type().const_int(0, false),
      Or => self.context.bool_type().const_int(1, false),
    };
    // create basic blocks
    let a_start_block = self.builder.get_insert_block().unwrap();
    let f = a_start_block.get_parent().unwrap();
    let b_start_block = self.context.append_basic_block(&f, "b_block");
    let end_block = self.context.append_basic_block(&f, "end");
    // compute a
    let a_value = self.codegen_int(a)?;
    let a_end_block = self.builder.get_insert_block().unwrap();
    match op {
      And => self.builder.build_conditional_branch(a_value, &b_start_block, &end_block),
      Or => self.builder.build_conditional_branch(a_value, &end_block, &b_start_block),
    };
    // maybe compute b
    self.builder.position_at_end(&b_start_block);
    let b_value = self.codegen_int(b)?;
    let b_end_block = self.builder.get_insert_block().unwrap();
    self.builder.build_unconditional_branch(&end_block);
    // end block
    self.builder.position_at_end(&end_block);
    let phi = self.builder.build_phi(self.context.bool_type(), "result");
    phi.add_incoming(&[
      (&short_circuit_outcome, &a_end_block),
      (&b_value, &b_end_block),
    ]);
    return Ok(reg(phi.as_basic_value()));
  }

  fn codegen_function_call(&mut self, ast : &AstNode, name : &RefStr, args : &[AstNode])
    -> Result<Option<JitVal>, Error>
  {
    let f =
      *self.functions.get(name)
      .ok_or_else(|| error_raw(ast.loc, format!("could not find function with name '{}'", name)))?;
    if f.count_params() as usize != args.len() {
        return error(ast.loc, "incorrect number of arguments passed");
    }
    // insert a prototype if needed
    let f = {
      if let Some(local_f) = self.module.get_function(name) {
        local_f
      }
      else{
        let def = self.function_defs.get(name).unwrap();
        let f = self.codegen_prototype(&def.name, &def.signature.return_type, &def.args, &def.signature.args);
        f.set_linkage(Linkage::External);
        f
      }
    };

    let mut arg_vals = vec!();
    for a in args.iter() {
      let v = self.codegen_value(a)?;
      arg_vals.push(v);
    }
    let call = self.builder.build_call(f, arg_vals.as_slice(), "tmp");
    println!("{:?}", call);
    let r = call.try_as_basic_value().left();
    return Ok(r.map(reg));
  }

  fn codegen_expression(&mut self, ast : &AstNode) -> Result<Option<JitVal>, Error> {
    let v : JitVal = match &ast.content {
      Content::FunctionCall(name, args) => {
        return self.codegen_function_call(ast, name, args);
      }
      Content::IntrinsicCall(name, args) => {
        if let [a, b] = args.as_slice() {
          match (&a.type_tag, name.as_ref()) {
          (Type::Float, "+") => binary_op!(build_float_add, FloatValue, a, b, self),
            (Type::Float, "-") => binary_op!(build_float_sub, FloatValue, a, b, self),
            (Type::Float, "*") => binary_op!(build_float_mul, FloatValue, a, b, self),
            (Type::Float, "/") => binary_op!(build_float_div, FloatValue, a, b, self),
            (Type::Float, ">") => compare_op!(build_float_compare, FloatPredicate::OGT, FloatValue, a, b, self),
            (Type::Float, ">=") => compare_op!(build_float_compare, FloatPredicate::OGE, FloatValue, a, b, self),
            (Type::Float, "<") => compare_op!(build_float_compare, FloatPredicate::OLT, FloatValue, a, b, self),
            (Type::Float, "<=") => compare_op!(build_float_compare, FloatPredicate::OLE, FloatValue, a, b, self),
            (Type::Float, "==") => compare_op!(build_float_compare, FloatPredicate::OEQ, FloatValue, a, b, self),
            (Type::I64, "+") => binary_op!(build_int_add, IntValue, a, b, self),
            (Type::I64, "-") => binary_op!(build_int_sub, IntValue, a, b, self),
            (Type::I64, "*") => binary_op!(build_int_mul, IntValue, a, b, self),
            //(Type::I64, "/") => binary_op!(build_int_div, IntValue, a, b, self),
            (Type::I64, ">") => compare_op!(build_int_compare, IntPredicate::SGT, IntValue, a, b, self),
            (Type::I64, ">=") => compare_op!(build_int_compare, IntPredicate::SGE, IntValue, a, b, self),
            (Type::I64, "<") => compare_op!(build_int_compare, IntPredicate::SLT, IntValue, a, b, self),
            (Type::I64, "<=") => compare_op!(build_int_compare, IntPredicate::SLE, IntValue, a, b, self),
            (Type::I64, "==") => compare_op!(build_int_compare, IntPredicate::EQ, IntValue, a, b, self),
            (Type::Bool, "&&") => self.codegen_short_circuit_op(a, b, ShortCircuitOp::And)?,
            (Type::Bool, "||") => self.codegen_short_circuit_op(a, b, ShortCircuitOp::Or)?,
            _ => return error(ast.loc, "encountered unrecognised intrinsic"),
          }        
        }
        else if let [a] = args.as_slice() {
          match (&a.type_tag, name.as_ref()) {
            (Type::Float, "unary_-") => unary_op!(build_float_neg, FloatValue, a, self),
            (Type::I64, "unary_-") => unary_op!(build_int_neg, IntValue, a, self),
            (Type::Bool, "unary_!") => unary_op!(build_not, IntValue, a, self),
            _ => return error(ast.loc, "encountered unrecognised intrinsic"),
          }
        }
        else {
          return error(ast.loc, "encountered unrecognised intrinsic");
        }
      }
      Content::While(ns) => {
        let (cond_node, body_node) = (&ns.0, &ns.1);
        let f = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let cond_block = self.context.append_basic_block(&f, "cond");
        let body_block = self.context.append_basic_block(&f, "loop_body");
        let exit_block = self.context.append_basic_block(&f, "loop_exit");
        let labels = LoopLabels { condition: cond_block, exit: exit_block };
        // jump to condition
        self.builder.build_unconditional_branch(&labels.condition);
        // conditional branch
        self.builder.position_at_end(&labels.condition);
        let cond_value = self.codegen_int(cond_node)?;
        self.builder.build_conditional_branch(cond_value, &body_block, &labels.exit);
        // loop body
        self.builder.position_at_end(&body_block);
        self.loop_labels.push(labels);
        self.codegen_expression(body_node)?;
        let labels = self.loop_labels.pop().unwrap();
        // loop back to start
        self.builder.build_unconditional_branch(&labels.condition);
        // exit
        self.builder.position_at_end(&labels.exit);
        return Ok(None);
      }
      Content::Break => {
        if let Some(labels) = self.loop_labels.last() {
          self.builder.build_unconditional_branch(&labels.exit);
          // create a dummy block to hold instructions after the break
          let f = self.builder.get_insert_block().unwrap().get_parent().unwrap();
          let dummy_block = self.context.append_basic_block(&f, "dummy_block");
          self.builder.position_at_end(&dummy_block);
          return Ok(None);
        }
        else {
          return error(ast.loc, "can only break inside a loop");
        }
      }
      Content::IfThen(ns) => {
        let (cond_node, then_node) = (&ns.0, &ns.1);
        let block = self.builder.get_insert_block().unwrap();
        let f = block.get_parent().unwrap();
        let then_block = self.context.append_basic_block(&f, "then");
        let end_block = self.context.append_basic_block(&f, "endif");
        // conditional branch
        let cond_value = self.codegen_int(cond_node)?;
        self.builder.build_conditional_branch(cond_value, &then_block, &end_block);
        // then block
        self.builder.position_at_end(&then_block);
        self.codegen_expression(then_node)?;
        self.builder.build_unconditional_branch(&end_block);
        // end block
        self.builder.position_at_end(&end_block);
        return Ok(None);
      }
      Content::IfThenElse(ns) => {
        let (cond_node, then_node, else_node) = (&ns.0, &ns.1, &ns.2);
        // create basic blocks
        let block = self.builder.get_insert_block().unwrap();
        let f = block.get_parent().unwrap();
        let then_start_block = self.context.append_basic_block(&f, "then");
        let else_start_block = self.context.append_basic_block(&f, "else");
        let end_block = self.context.append_basic_block(&f, "endif");
        // conditional branch
        let cond_value = self.codegen_int(cond_node)?;
        self.builder.build_conditional_branch(
          cond_value, &then_start_block, &else_start_block);
        // then block
        self.builder.position_at_end(&then_start_block);
        let then_value = self.codegen_expression_to_register(then_node)?;
        let then_end_block = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(&end_block);
        // else block
        self.builder.position_at_end(&else_start_block);
        let else_value = self.codegen_expression_to_register(else_node)?;
        let else_end_block = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(&end_block);
        // end block
        self.builder.position_at_end(&end_block);
        if then_value.is_some() && else_value.is_some() {
          let v1 = then_value.unwrap();
          let v2 = else_value.unwrap();
          let phi = self.builder.build_phi(v1.get_type(), "if_result");
          phi.add_incoming(&[
            (&v1, &then_end_block),
            (&v2, &else_end_block),
          ]);
          reg(phi.as_basic_value())
        }
        else {
          return Ok(None);
        }
      }
      Content::Block(nodes) => {
        let node_count = nodes.len();
        if node_count > 0 {
          for i in 0..(node_count-1) {
            self.codegen_expression(&nodes[i])?;
          }
          return self.codegen_expression(&nodes[node_count-1]);
        }
        return Ok(None);
      }
      Content::CFunctionPrototype(def) => {
        let f = self.codegen_prototype(&def.name, &def.signature.return_type, &def.args, &def.signature.args);
        self.functions.insert(def.name.clone(), f);
        return Ok(None);
      }
      Content::FunctionDefinition(def, body) => {
        let global_var_types = HashMap::new();
        self.child_function_jit(&global_var_types).codegen_function(
          ast, body, &def.name, &def.args, &def.signature.args, false)?;
        return Ok(None);
      }
      Content::StructDefinition(_def) => {
        // TODO: is nothing required here?
        return Ok(None);
      }
      Content::StructInstantiate(def, args) => {
        let t = self.struct_type(def).as_basic_type_enum();
        let ptr = self.create_entry_block_alloca(t, &def.name);
        for (i, a) in args.iter().enumerate() {
          let v = self.codegen_value(a)?;
          let element_ptr = unsafe {
            self.builder.build_struct_gep(ptr, i as u32, &def.fields[i].0)
          };
          self.builder.build_store(element_ptr, v);
        }
        stack(ptr)
      }
      Content::FieldAccess(x, field_index) => {
        let (struct_val_node, field_name) = (&x.0, &x.1);
        let v = self.codegen_expression(struct_val_node)?.unwrap();
        match v.storage {
          Storage::Register => {
            // if the struct is in a register, dereference the field into a register
            let reg_val =
              self.builder.build_extract_value(
                *v.value.as_struct_value(), *field_index as u32, field_name).unwrap();
            reg(reg_val)
          }
          Storage::Stack => {
            // if this is a pointer to the struct, get a pointer to the field
            let ptr = *v.value.as_pointer_value();
            let field_ptr = unsafe {
              self.builder.build_struct_gep(ptr, *field_index as u32, field_name)
            };
            stack(field_ptr)
          }
        }
      }
      Content::Assignment(ns) => {
        let (assign_node, value_node) = (&ns.0, &ns.1);
        let assign_location = self.codegen_expression(assign_node)?.unwrap();
        let ptr = match assign_location.storage {
          Storage::Stack => {
            *assign_location.value.as_pointer_value()
          }
          Storage::Register => {
            return error(assign_node.loc, "cannot assign to this construct");
          }
        };
        let reg_val = self.codegen_expression_to_register(value_node)?.unwrap();
        self.builder.build_store(ptr, reg_val);
        return Ok(None);
      }
      Content::VariableInitialise(name, value_node, var_scope) => {
        let v = self.codegen_expression_to_register(value_node)?
          .ok_or_else(|| error_raw(value_node.loc, "expected value for initialiser, found void"))?;
        self.init_variable(name.clone(), *var_scope, v)
          .map_err(|c| error_raw(ast.loc, c))?; 
        return Ok(None);
      }
      Content::VariableReference(name) => {
        if let Some(ptr) = self.variables.get(name) {
          stack(*ptr)
        }
        else if let Some(type_tag) = self.global_var_types.get(name) {
          let t = self.to_basic_type(type_tag).unwrap();
          let gv = self.module.add_global(t, Some(AddressSpace::Global), &name);
          gv.set_constant(false);
          let ptr = gv.as_pointer_value();
          self.variables.insert(name.clone(), ptr);
          stack(ptr)
        }
        else {
          return error(ast.loc, format!("unknown variable name '{}'.", name));
        }
      }
      Content::Deref(_n) => {
        /*
        TODO
        let ptr =
          self.codegen_pointer(n)?
          .ok_or_else(|| error_raw(n.loc, "cannot dereference this construct"))?;
        reg(self.builder.build_load(ptr, "deref"))
        */
        panic!();
      }
      Content::ExplicitReturn(n) => {
        self.codegen_return(n.as_ref().map(|b| b as &AstNode))?;
        // create a dummy block to hold instructions after the return
        let f = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let dummy_block = self.context.append_basic_block(&f, "dummy_block");
        self.builder.position_at_end(&dummy_block);
        return Ok(None);
      }
      Content::Literal(v) => {
        match v {
          Val::Float(f) => reg(self.context.f64_type().const_float(*f).into()),
          Val::I64(i) => reg(self.context.i64_type().const_int(*i as u64, false).into()),
          Val::Bool(b) =>
            reg(self.context.bool_type().const_int(if *b { 1 } else { 0 }, false).into()),
          Val::Void => return Ok(None),
        }
      }
    };
    Ok(Some(v))
  }

  fn name_basic_type(t : &BasicValueEnum, s : &str) {
    use BasicValueEnum::*;
    match t {
      ArrayValue(v) => v.set_name(s),
      IntValue(v) => v.set_name(s),
      FloatValue(v) => v.set_name(s),
      PointerValue(v) => v.set_name(s),
      StructValue(v) => v.set_name(s),
      VectorValue(v) => v.set_name(s),
    }
  }

  fn to_basic_type(&mut self, t : &Type) -> Option<BasicTypeEnum> {
    match t {
      Type::Void => None,
      Type::Float => Some(self.context.f64_type().into()),
      Type::I64 => Some(self.context.i64_type().into()),
      Type::Bool => Some(self.context.bool_type().into()),
      Type::Struct(def) => Some(self.struct_type(def).as_basic_type_enum()),
      Type::Ptr(t) => {
        let bt = self.to_basic_type(t);
        Some(self.pointer_to_type(bt).into())
      }
    }
  }

  fn pointer_to_type(&self, t : Option<BasicTypeEnum>) -> PointerType {
    if let Some(t) = t {
    use BasicTypeEnum::*;
      match t {
        ArrayType(t) => t.ptr_type(AddressSpace::Local),
        IntType(t) => t.ptr_type(AddressSpace::Local),
        FloatType(t) => t.ptr_type(AddressSpace::Local),
        PointerType(t) => t.ptr_type(AddressSpace::Local),
        StructType(t) => t.ptr_type(AddressSpace::Local),
        VectorType(t) => t.ptr_type(AddressSpace::Local),
      }
    }
    else {
      self.context.void_type().ptr_type(AddressSpace::Local)
    }
  }

  fn function_type(&self, return_type : Option<BasicTypeEnum>, arg_types : &[BasicTypeEnum])
    -> FunctionType
  {
    if let Some(return_type) = return_type {
      use BasicTypeEnum::*;
      match return_type {
        ArrayType(t) => t.fn_type(arg_types, false),
        IntType(t) => t.fn_type(arg_types, false),
        FloatType(t) => t.fn_type(arg_types, false),
        PointerType(t) => t.fn_type(arg_types, false),
        StructType(t) => t.fn_type(arg_types, false),
        VectorType(t) => t.fn_type(arg_types, false),
      }
    }
    else {
      self.context.void_type().fn_type(arg_types, false)
    }
  }

  fn struct_type(&mut self, def : &StructDefinition) -> StructType {
    if let Some(t) = self.struct_types.get(&def.name) {
      return *t;
    }
    let types =
      def.fields.iter().map(|(_, t)| {
        self.to_basic_type(t).unwrap()
      })
      .collect::<Vec<BasicTypeEnum>>();
    let t = self.context.struct_type(&types, false);
    self.struct_types.insert(def.name.clone(), t);
    return t;
  }

  fn codegen_return(&mut self, value_node : Option<&AstNode>) -> Result<(), Error> {
    if let Some(value_node) = value_node {
      let v = self.codegen_expression_to_register(value_node)?;
      self.builder.build_return(v.as_ref().map(|v| v as &BasicValue));
    }
    else {
      self.builder.build_return(None);
    }
    Ok(())
  }

  fn codegen_prototype(
    &mut self, name : &str,
    return_type : &Type,
    args : &[RefStr],
    arg_types : &[Type])
      -> FunctionValue
  {
    let arg_types =
      arg_types.iter().map(|t| self.to_basic_type(t).unwrap())
      .collect::<Vec<BasicTypeEnum>>();
    let arg_types = arg_types.as_slice();

    let return_type = self.to_basic_type(return_type);
    let fn_type = self.function_type(return_type, arg_types);
    let function = self.module.add_function(name, fn_type, Some(Linkage::External));

    // set arguments names
    for (i, arg) in function.get_param_iter().enumerate() {
      Jit::name_basic_type(&arg, args[i].as_ref());
    }
    function
  }

  fn codegen_function(
    mut self,
    function_node : &AstNode,
    body : &AstNode,
    name : &str,
    args : &[RefStr],
    arg_types : &[Type],
    is_top_level : bool)
      -> Result<FunctionValue, Error>
  {
    let function = self.codegen_prototype(name, &body.type_tag, args, arg_types);

    if !is_top_level {
      self.functions.insert(name.into(), function);
    }

    // this function is here because Rust doesn't have a proper try/catch yet
    fn generate(
      function_node : &AstNode, body : &AstNode, function : FunctionValue,
      args : &[RefStr], jit : &mut Jit) -> Result<(), Error>
    {
      let entry = jit.context.append_basic_block(&function, "entry");

      jit.builder.position_at_end(&entry);

      // set function parameters
      for (arg_value, arg_name) in function.get_param_iter().zip(args) {
        jit.init_variable(arg_name.clone(), VarScope::Local, arg_value)
          .map_err(|c| error_raw(function_node.loc, c))?;
      }

      // compile body and emit return
      jit.codegen_return(Some(body))?;

      // return the whole thing after verification and optimization
      if function.verify(true) {
        jit.pm.run_on_function(&function);
        Ok(())
      }
      else {
        error(function_node.loc, "invalid generated function.")
      }
    }

    match generate(function_node, body, function, args, &mut self) {
      Ok(_) => Ok(function),
      Err(e) => {
        dump_module(self.module);
        // This library uses copy semantics for a resource can be deleted,
        // because it is usually not deleted. As a result, it's possible to
        // get use-after-free bugs, so this operation is unsafe. I'm sure
        // this design could be improved.
        unsafe {
          function.delete();
        }
        Err(e)
      }
    }
  }

  fn codegen_top_level(self, ast : &AstNode) -> Result<FunctionValue, Error> {
    self.codegen_function(&ast, &ast, "top_level", &[], &[], true)
  }
}

pub struct Interpreter {
  sym : SymbolTable,
  context : Context,
  modules : Vec<(Module, ExecutionEngine)>,
  functions : HashMap<RefStr, Rc<FunctionDefinition>>,
  compiled_functions : HashMap<RefStr, FunctionValue>,
  struct_types : HashMap<RefStr, Rc<StructDefinition>>,
  global_var_types: HashMap<RefStr, Type>,
}

impl Interpreter {
  pub fn new() -> Interpreter {
    let sym = SymbolTable::new();
    let context = Context::create();
    let functions = HashMap::new();
    let compiled_functions = HashMap::new();
    let struct_types = HashMap::new();
    let global_var_types = HashMap::new();

    let modules = vec!();

    let mut i = 
      Interpreter {
        sym, context, modules, functions,
        compiled_functions, struct_types,
        global_var_types };
    
    // load prelude
    if let Err(e) = i.load_prelude() {
      println!("error loading prelude, {}", e);
    }
    
    return i;
  }

  fn load_module(&mut self, code : &str) -> Result<(), Error> {
    let expr = self.parse_string(code)?;
    self.run_expression(&expr)?;
    Ok(())
  }

  fn load_prelude(&mut self) -> Result<(), Error> {
    let mut f = File::open("code/prelude_llvm.code").expect("file not found");
    let mut code = String::new();
    f.read_to_string(&mut code).unwrap();
    self.load_module(&code)?;
    Ok(())
  }

  fn top_level_jit<'l>(&'l mut self, module : &'l mut Module, pm : &'l mut PassManager) -> Jit<'l> {
    Jit::new(
      &mut self.context, module, &mut self.compiled_functions,
      &self.functions, &self.global_var_types, pm)
  }

  fn parse_string(&mut self, code : &str) -> Result<Expr, Error> {
    let tokens =
      lexer::lex(code, &mut self.sym)
      .map_err(|mut es| es.remove(0))?;
    parser::parse(tokens, &mut self.sym)
  }

  pub fn run(&mut self, code : &str) -> Result<Val, Error> {;
    let expr = self.parse_string(code)?;
    self.run_expression(&expr)
  }

  fn run_expression(&mut self, expr : &Expr) -> Result<Val, Error> {
    let mut type_checker =
      TypeChecker::new(
        true, HashMap::new(), &mut self.functions, &mut self.struct_types,
        &mut self.global_var_types, &mut self.sym);
    let ast = type_checker.to_ast(expr)?;
    let module_name = format!("module_{}", self.modules.len());
    let mut module = self.context.create_module(&module_name);
    let mut pm = PassManager::create_for_function(&module);
    // TODO: decide what to do about optimisation
    pm.add_instruction_combining_pass();
    pm.add_reassociate_pass();
    pm.add_gvn_pass();
    pm.add_cfg_simplification_pass();
    pm.add_basic_alias_analysis_pass();
    pm.add_promote_memory_to_register_pass();
    pm.add_instruction_combining_pass();
    pm.add_reassociate_pass();  
    pm.initialize();

    let f = {
      let jit = self.top_level_jit(&mut module, &mut pm);
      jit.codegen_top_level(&ast)?
    };
    println!("{}", display_expr(expr));
    dump_module(&module);

    fn execute<T>(expr : &Expr, f : FunctionValue, ee : &ExecutionEngine) -> Result<T, Error> {
      let function_name = f.get_name().to_str().unwrap();
      let v = unsafe {
        // ERROR_MAY_BE_HERE; 
        // TODO: i'm not sure that "get_function" actually calls "FinalizeObject"
        // see: https://llvm.org/doxygen/ExecutionEngineBindings_8cpp_source.html
        let jit_function =
          ee.get_function::<unsafe extern "C" fn() -> T>(function_name)
          .map_err(|e| error_raw(expr, format!("{:?}", e)))?;
        jit_function.call()
      };
      Ok(v)
    }
    let ee =
      module.create_jit_execution_engine(OptimizationLevel::None)
      .map_err(|e| error_raw(expr, e.to_string()))?;
    ee.run_static_constructors(); // TODO: this might not do anything :(
    let result = match ast.type_tag {
      Type::Bool => execute::<bool>(expr, f, &ee).map(Val::Bool),
      Type::Float => execute::<f64>(expr, f, &ee).map(Val::Float),
      Type::I64 => execute::<i64>(expr, f, &ee).map(Val::I64),
      Type::Void => execute::<()>(expr, f, &ee).map(|_| Val::Void),
      Type::Ptr(_) => 
        error(expr, "can't return a pointer from a top-level function"),
      Type::Struct(_) =>
        error(expr, "can't return a struct from a top-level function"),
    };
    // unsafe { f.delete(); }
    // TODO: ee.remove_module(&i.module).unwrap();
    self.modules.push((module, ee));
    result
  }
}

#[no_mangle]
pub extern "C" fn blah(a : i64, b : i64) -> i64 {
  a + b
}

// Adding the functions above to a global array,
// so Rust compiler won't remove them.
#[used]
static EXTERNAL_FNS: [extern fn(i64, i64) -> i64; 1] = [blah];

pub fn run_repl() {
  dlltest::blahblah(4, 5);

  let mut rl = Editor::<()>::new();
  let mut i = Interpreter::new();

  unsafe {
    LLVMLoadLibraryPermanently(std::ptr::null());
  }

  loop {
    let mut input_line = rl.readline("repl> ").unwrap();

    loop {
      let lex_result =
        lexer::lex(input_line.as_str(), &mut i.sym)
        .map_err(|mut es| es.remove(0));
      let tokens = match lex_result {
        Ok(tokens) => tokens,
        Err(e) => {
          println!("Error occured: {}", e);
          break;
        }
      };
      let parsing_result = parser::repl_parse(tokens, &mut i.sym);
      match parsing_result {
        Ok(Complete(e)) => {
          // we have parsed a full expression
          rl.add_history_entry(input_line);
          match i.run_expression(&e) {
            Ok(value) => {
              println!("{:?}", value)
            }
            Err(err) => {
              println!("error: {}", err);
            }
          }
          break;
        }
        Ok(Incomplete) => {
          // get more tokens
          let next_line = rl.readline(". ").unwrap();
          input_line.push_str("\n");
          input_line.push_str(next_line.as_str());
        }
        Err(e) => {
          println!("Error occured: {}", e);
          break;
        }
      }
    }
  }
}
