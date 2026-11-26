//! Port of KaTeX `src/Namespace.ts`, specialised to macros: the built-ins are the static table
//! in `data/macros_data.rs`; `current` holds what the formula itself defines (`\def`,
//! `\newcommand`, KaTeX-internal `\df@tag`, …).

use std::collections::HashMap;

use crate::macros::{MacroDef, builtin};
use crate::parse_error::{Result, ierr};

#[derive(Default)]
pub struct Namespace {
    pub current: HashMap<String, MacroDef>,
    undef_stack: Vec<HashMap<String, Option<MacroDef>>>,
}

impl Namespace {
    pub fn begin_group(&mut self) {
        self.undef_stack.push(HashMap::new());
    }

    pub fn end_group(&mut self) -> Result<()> {
        let Some(undefs) = self.undef_stack.pop() else {
            return ierr(
                "Unbalanced namespace destruction: attempt to pop global namespace; please report this as a bug",
            );
        };
        for (key, value) in undefs {
            match value {
                None => {
                    self.current.remove(&key);
                }
                Some(v) => {
                    self.current.insert(key, v);
                }
            }
        }
        Ok(())
    }

    pub fn end_groups(&mut self) {
        while !self.undef_stack.is_empty() {
            let _ = self.end_group();
        }
    }

    pub fn has(&self, name: &str) -> bool {
        self.current.contains_key(name) || builtin(name).is_some()
    }

    pub fn get(&self, name: &str) -> Option<MacroDef> {
        if let Some(v) = self.current.get(name) {
            Some(v.clone())
        } else {
            builtin(name).map(MacroDef::from_builtin)
        }
    }

    /// `set(name, value, global)`; `None` deletes from `current`.
    pub fn set(&mut self, name: &str, value: Option<MacroDef>, global: bool) {
        if global {
            for frame in self.undef_stack.iter_mut() {
                frame.remove(name);
            }
            if let Some(top) = self.undef_stack.last_mut() {
                top.insert(name.to_string(), value.clone());
            }
        } else if let Some(top) = self.undef_stack.last_mut()
            && !top.contains_key(name)
        {
            top.insert(name.to_string(), self.current.get(name).cloned());
        }
        match value {
            None => {
                self.current.remove(name);
            }
            Some(v) => {
                self.current.insert(name.to_string(), v);
            }
        }
    }
}
