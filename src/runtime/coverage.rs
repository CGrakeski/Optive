//! `Optive test --cover`：按源码行记命中。不进 `dispatch_hot_u8`。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::debug;
use crate::opcode::{CompiledProgram, FunctionObject, Instruction};
use crate::shared::Shared;
use crate::value::Value;
use crate::vm::Vm;

#[derive(Debug, Default)]
pub struct CoverageState {
    pub hits: HashSet<(String, usize)>,
    pub executable: HashSet<(String, usize)>,
    files: HashSet<String>,
    root: Option<PathBuf>,
    norm_cache: HashMap<String, String>,
}

impl CoverageState {
    #[must_use]
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = root.canonicalize().unwrap_or(root);
        Self {
            root: Some(root),
            ..Self::default()
        }
    }

    fn normalize_file(&self, file: &str) -> String {
        if file.is_empty() || file.starts_with('<') {
            return file.replace('\\', "/");
        }
        let Some(root) = &self.root else {
            return debug::normalize_path(file);
        };
        let path = Path::new(file);
        let full = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let full = full.canonicalize().unwrap_or(full);
        full.strip_prefix(root)
            .unwrap_or(&full)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn cached_normalize(&mut self, file: &str) -> String {
        if let Some(hit) = self.norm_cache.get(file) {
            return hit.clone();
        }
        let norm = self.normalize_file(file);
        self.norm_cache.insert(file.to_string(), norm.clone());
        norm
    }

    pub fn hit(&mut self, file: &str, line: usize) {
        if line == 0 {
            return;
        }
        let file = self.cached_normalize(file);
        self.files.insert(file.clone());
        self.hits.insert((file, line));
    }

    pub fn note_lines(&mut self, file: &str, lines: &[usize]) {
        let file = self.cached_normalize(file);
        self.files.insert(file.clone());
        for &line in lines {
            if line > 0 {
                self.executable.insert((file.clone(), line));
            }
        }
    }

    pub fn note_program(&mut self, file: &str, prog: &CompiledProgram) {
        let normalized = self.cached_normalize(file);
        self.files.insert(normalized);
        self.note_lines(file, &prog.line_map);
        let mut visited = HashSet::new();
        self.note_instructions(&prog.code, &mut visited);
        for f in prog.functions.values() {
            self.note_function(f, &mut visited);
        }
        for overloads in prog.overload_tables.values() {
            for f in overloads {
                self.note_function(f, &mut visited);
            }
        }
    }

    fn note_instructions(&mut self, code: &[Instruction], visited: &mut HashSet<usize>) {
        for instruction in code {
            if let Instruction::Push(Value::Function(function)) = instruction {
                self.note_function(function, visited);
            }
        }
    }

    fn note_function(&mut self, f: &FunctionObject, visited: &mut HashSet<usize>) {
        let address = std::ptr::from_ref(f) as usize;
        if !visited.insert(address) {
            return;
        }
        let file = if f.source_file.is_empty() {
            return;
        } else {
            f.source_file.as_str()
        };
        self.note_lines(file, &f.line_map);
        self.note_instructions(&f.body, visited);
    }

    pub fn per_file(&self) -> BTreeMap<String, (usize, usize)> {
        let mut files: BTreeMap<String, (HashSet<usize>, HashSet<usize>)> = BTreeMap::new();
        for file in &self.files {
            files.entry(file.clone()).or_default();
        }
        for (f, line) in &self.executable {
            files.entry(f.clone()).or_default().1.insert(*line);
        }
        for (f, line) in &self.hits {
            files.entry(f.clone()).or_default().0.insert(*line);
            files.entry(f.clone()).or_default().1.insert(*line);
        }
        files
            .into_iter()
            .map(|(f, (hit, exec))| (f, (hit.len(), exec.len())))
            .collect()
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut files = serde_json::Map::new();
        for (file, (hit, exec)) in self.per_file() {
            let pct = if exec == 0 {
                None
            } else {
                Some((hit as f64) * 100.0 / (exec as f64))
            };
            files.insert(
                file,
                serde_json::json!({ "hit": hit, "exec": exec, "pct": pct }),
            );
        }
        serde_json::json!({ "files": files })
    }

    pub fn write_report(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(&self.to_json())?)
    }
}

pub fn attach(vm: &mut Vm, state: Shared<CoverageState>) {
    vm.cover = Some(state);
    vm.cover_active = true;
}

pub fn record_hit(vm: &Vm) {
    let Some(cov) = &vm.cover else {
        return;
    };
    let line = debug::line_at_pc(vm);
    if line == 0 {
        return;
    }
    let (file, _) = debug::current_location(vm);
    cov.borrow_mut().hit(&file, line);
}

pub fn note_compiled(vm: &Vm, file: &str, prog: &CompiledProgram) {
    let Some(cov) = &vm.cover else {
        return;
    };
    cov.borrow_mut().note_program(file, prog);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_empty_program_is_not_reported_as_one_executable_line() {
        let mut coverage = CoverageState::default();
        coverage.note_program("empty.tive", &CompiledProgram::default());

        assert_eq!(coverage.per_file().get("empty.tive"), Some(&(0, 0)));
        let json = coverage.to_json();
        assert!(json["files"]["empty.tive"]["pct"].is_null());
        assert!(json["files"].get("unknown.tive").is_none());
    }

    #[test]
    fn uncalled_function_lines_are_executable() {
        let vm = Vm::new();
        let source = "export func idle() {\n    let untouched = 41\n    return untouched + 1\n}\n";
        let program =
            crate::compile_with_context(&vm, source, "src/lib.tive").expect("compile source");
        let pushed: Vec<_> = program
            .code
            .iter()
            .filter_map(|instruction| match instruction {
                Instruction::Push(Value::Function(function)) => Some((
                    function.source_file.clone(),
                    function.line_map.as_ref().clone(),
                )),
                _ => None,
            })
            .collect();
        let tabled: Vec<_> = program
            .functions
            .values()
            .map(|function| {
                (
                    function.source_file.clone(),
                    function.line_map.as_ref().clone(),
                )
            })
            .collect();
        let mut coverage = CoverageState::default();
        coverage.note_program("src/lib.tive", &program);
        let (_, executable) = coverage.per_file()["src/lib.tive"];
        assert!(
            executable > 1,
            "uncalled function lines missing; pushed={pushed:?}, tabled={tabled:?}"
        );
    }
}
