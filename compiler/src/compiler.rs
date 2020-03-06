
use crate::{
  error, expr, c_interface, llvm_compile, types, code_store,
  structure, lexer, parser, inference_solver, intrinsics, graph,
};
use expr::{StringCache, Expr, UIDGenerator };
use c_interface::CSymbols;
use code_store::{CodeStore, SourceId};
use types::{TypeContent, PType, UnitId};
use llvm_compile::{LlvmCompiler, execute_function};
use error::{Error, error};
use structure::TOP_LEVEL_FUNCTION_NAME;
use graph::DirectedGraph;

use std::collections::{VecDeque, HashSet};

// TODO: Put these options somewhere more sensible
pub static DEBUG_PRINTING_IR : bool = false;
pub static ENABLE_IR_OPTIMISATION : bool = false;
pub static DEBUG_PRINTING_DEPENDENCY_GRAPH : bool = false;
pub static DEBUG_PRINTING_TYPE_INFERENCE : bool = false;

pub struct Compiler {
  pub code_store : CodeStore,
  pub llvm_compiler : LlvmCompiler,
  pub gen : UIDGenerator,
  pub cache : StringCache,
  pub c_symbols : CSymbols,
  intrinsics : UnitId,
}

impl Compiler {
  pub fn new() -> Box<Compiler> {
    let mut gen = UIDGenerator::new();
    let cache = StringCache::new();
    let mut code_store  = CodeStore::new();
    let intrinsics_id = code_store.create_unit(gen.next(), Some(cache.get("intrinsics")));
    let i_types = intrinsics::get_intrinsics(intrinsics_id, &mut gen, &cache);
    code_store.types.insert(intrinsics_id, i_types);
    let llvm_compiler = LlvmCompiler::new();
    let c_symbols = CSymbols::new_populated();
    let mut c = Box::new(Compiler { 
      code_store, llvm_compiler, gen, cache,
      c_symbols, intrinsics: intrinsics_id,
    });
    let cptr = (&mut *c) as *mut Compiler;
    c.c_symbols.add_symbol("compiler", cptr);
    c
  }

  pub fn load_expr_as_module(&mut self, expr : &Expr, name : Option<&str>, imports : &[UnitId])
    -> Result<(UnitId, Val), Error>
  {
    let name = name.map(|s| self.cache.get(s));
    let unit_id = self.code_store.create_unit(self.gen.next(), name);
    self.code_store.exprs.insert(unit_id, expr.clone());
    self.load_module_from_expr_internal(unit_id, imports.iter().cloned().collect())?;
    let val = self.code_store.vals.get(&unit_id).unwrap().clone();
    Ok((unit_id, val))
  }

  pub fn load_module(&mut self, code : &str, name : Option<&str>, imports : &[UnitId])
    -> Result<(UnitId, Val), Error>
  {
    let name = name.map(|s| self.cache.get(s));
    let unit_id = self.code_store.create_unit(self.gen.next(), name);
    let source_id = self.gen.next().into();
    self.code_store.code.insert(source_id, code.into());
    self.parse(source_id, unit_id)?;
    self.load_module_from_expr_internal(unit_id, imports.iter().cloned().collect())?;
    let val = self.code_store.vals.get(&unit_id).unwrap().clone();
    Ok((unit_id, val))
  }

  fn parse(&mut self, source_id : SourceId, unit_id : UnitId) -> Result<(), Error> {
    let code = self.code_store.code.get(&source_id).unwrap();
    let tokens =
      lexer::lex(&code, &self.cache)
      .map_err(|mut es| es.remove(0))?;
    let expr = parser::parse(tokens, &self.cache)?;
    self.code_store.exprs.insert(unit_id, expr);
    Ok(())
  }

  fn load_module_from_expr_internal(&mut self, unit_id : UnitId, mut imports : HashSet<UnitId>)
    -> Result<(), Error>
  {
    imports.insert(self.intrinsics);
    for &i in imports.iter() {
      self.code_store.add_dependency(unit_id, i);
    }
    self.structure(unit_id)?;
    let mut new_units = vec![unit_id];
    self.typecheck(unit_id, imports, &mut new_units)?;
    self.codegen(new_units.as_slice())?;
    self.initialise(unit_id)?;
    Ok(())
  }

  fn structure(&mut self, unit_id : UnitId) -> Result<(), Error> {
    let expr = self.code_store.exprs.get(&unit_id).unwrap();
    let nodes = structure::to_nodes(&mut self.gen, &self.cache, &expr)?;
    self.code_store.nodes.insert(unit_id, nodes);
    Ok(())
  }

  fn typecheck(&mut self, unit_id : UnitId, imports : HashSet<UnitId>, new_units : &mut Vec<UnitId>) -> Result<(), Error> {
    let (types, mapping) =
      inference_solver::infer_types(
        unit_id, &self.code_store, &self.cache, &mut self.gen, &imports)?;
        self.code_store.types.insert(unit_id, types);
        self.code_store.type_mappings.insert(unit_id, mapping);
    self.typecheck_new_polymorphic_instances(unit_id, new_units)?;
    Ok(())
  }

  fn typecheck_new_polymorphic_instances(&mut self, unit_id : UnitId, new_units : &mut Vec<UnitId>) -> Result<(), Error> {
    // Typecheck any new polymorphic function instances
    let mut search_queue = VecDeque::new();
    search_queue.push_back(unit_id);
    while let Some(psid) = search_queue.pop_front() {
      let mapping = self.code_store.type_mappings.get(&psid).unwrap();
      let polymorphic_references : Vec<_> = mapping.polymorphic_references.iter().cloned().collect();
      for (poly_symbol_id, instance_type) in polymorphic_references {
        let existing_poly_instance = self.code_store.poly_instance(poly_symbol_id, &instance_type);
        if let Some(id) = existing_poly_instance {
          self.code_store.add_dependency(psid, id.uid);
        }
        else {
          // Create a new unit for the function instance and typecheck it
          let poly_unit_name = {
            let name = self.code_store.symbol_def(poly_symbol_id).name.as_ref();
            self.cache.get(format!("@poly[{}][{}]", name, instance_type))
          };
          let instance_unit_id = self.code_store.create_unit(self.gen.next(), Some(poly_unit_name));
          let mut instance_dependencies = self.code_store.dependencies(poly_symbol_id.uid).clone();
          instance_dependencies.insert(poly_symbol_id.uid);
          self.code_store.dependencies.insert(instance_unit_id, instance_dependencies);
          let poly_def = self.code_store.symbol_def(poly_symbol_id);
          let (instance_types, instance_mapping, instance_symbol_id) =
            inference_solver::typecheck_polymorphic_function_instance(
              instance_unit_id, poly_def, &instance_type, &self.code_store,
              &self.cache, &mut self.gen)?;
          // Register the instance with the code store
          let instances = self.code_store.poly_instances.entry(poly_symbol_id).or_default();
          instances.insert(instance_type, instance_symbol_id);
          self.code_store.poly_parents.insert(instance_unit_id, poly_symbol_id);
          self.code_store.types.insert(instance_unit_id, instance_types);
          self.code_store.type_mappings.insert(instance_unit_id, instance_mapping);
          // Register the new unit
          new_units.push(instance_unit_id);
          search_queue.push_back(instance_unit_id);
          // Register the dependency
          self.code_store.add_dependency(psid, instance_unit_id);
        }
      }
    }
    Ok(())
  }

  fn codegen(&mut self, new_units : &[UnitId]) -> Result<(), Error> {
    if DEBUG_PRINTING_DEPENDENCY_GRAPH {
      println!("units {{");
      for (i, u) in new_units.iter().cloned().enumerate() {
        let name = self.code_store.name(u);
        println!("  {}: {}", i, name);
      }
      println!("}}");
    }
    // Use Tarjan's algorithm to get a DAG of the "strongly-connected-components".
    // Codegen these groups together in a valid order.
    let mut g : DirectedGraph = Default::default();
    for uid in new_units.iter() {
      let mut vertex_edges = vec![];
      for d in self.code_store.dependencies.get(uid).unwrap() {
        if let Some(w) = new_units.iter().position(|id| id == d) {
          vertex_edges.push(w);
        }
      }
      g.vertex_edges.push(vertex_edges);
    }
    if DEBUG_PRINTING_DEPENDENCY_GRAPH {
      println!("unit_graph {}", g);
    }
    let strongly_connected_components = graph::get_strongly_connected_components(&g);
    if DEBUG_PRINTING_DEPENDENCY_GRAPH {
      println!("components {{");
      for c in strongly_connected_components.iter() {
        println!("  {:?}", c);
      }
      println!("}}");
    }
    let ordering = {
      let component_graph = graph::graph_of_disjoint_subgraphs(strongly_connected_components.as_slice(), &g);
      if DEBUG_PRINTING_DEPENDENCY_GRAPH {
        println!("component_graph {}", component_graph);
      }
      graph::valid_topological_ordering(&component_graph).expect("graph contained cycles!")
    };
    if DEBUG_PRINTING_DEPENDENCY_GRAPH {
      println!("ordering: {:?}", ordering);
    }
    // Codegen the strongly-connected subgraphs together
    let mut unit_group = vec![];
    for subgraph_index in ordering {
      let g = &strongly_connected_components[subgraph_index];
      // build unit group
      unit_group.clear();
      for &i in g {
        unit_group.push(new_units[i]);
      }
      // codegen group
      let codegen_id = self.gen.next().into();
      let lu = self.llvm_compiler.compile_unit_group(codegen_id, unit_group.as_slice(), &self.code_store)?;
      for &unit_id in unit_group.iter() {
        self.code_store.codegen_mapping.insert(unit_id, codegen_id);
      }
      self.code_store.llvm_units.insert(codegen_id, lu);
      llvm_compile::link_unit(codegen_id, &self.code_store, &self.c_symbols);
    }
    Ok(())
  }

  fn initialise(&mut self, unit_id : UnitId) -> Result<(), Error> {
    let val = self.run_top_level(unit_id)?;
    self.code_store.vals.insert(unit_id, val);
    Ok(())
  }

  fn run_top_level(&self, unit_id : UnitId) -> Result<Val, Error> {
    use TypeContent::*;
    use PType::*;
    let f = TOP_LEVEL_FUNCTION_NAME;
    let types = self.code_store.types(unit_id);
    let def = types.symbols.values().find(|def| def.name.as_ref() == f).unwrap();
    let f = def.codegen_name().unwrap();
    let sig = if let Some(sig) = def.type_tag.sig() {sig} else {panic!()};
    let lu = self.code_store.llvm_unit(unit_id);
    let value = match &sig.return_type.content {
      Prim(Bool) => Val::Bool(execute_function(f, lu)),
      Prim(F64) => Val::F64(execute_function(f, lu)),
      Prim(F32) => Val::F32(execute_function(f, lu)),
      Prim(I64) => Val::I64(execute_function(f, lu)),
      Prim(I32) => Val::I32(execute_function(f, lu)),
      Prim(U64) => Val::U64(execute_function(f, lu)),
      Prim(U32) => Val::U32(execute_function(f, lu)),
      Prim(U16) => Val::U16(execute_function(f, lu)),
      Prim(U8) => Val::U8(execute_function(f, lu)),
      Prim(Void) => {
        execute_function::<()>(f, lu);
        Val::Void
      }
      t => {
        let loc = self.code_store.nodes(unit_id).root().loc;
        return error(loc, format!("can't return value of type {:?} from a top-level function", t));
      }
    };
    Ok(value)
  }

}

#[derive(Clone, PartialEq, Debug)]
pub enum Val {
  Void,
  F64(f64),
  F32(f32),
  I64(i64),
  U64(u64),
  I32(i32),
  U32(u32),
  U16(u16),
  U8(u8),
  String(String),
  Bool(bool),
}
