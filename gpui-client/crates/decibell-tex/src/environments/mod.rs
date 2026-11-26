//! Port of KaTeX `src/environments.ts` / `src/defineEnvironment.ts`: the environment
//! name → spec lookup (`_environments`).

use crate::define_function::EnvSpec;

pub mod array;
pub mod cd;

static ALL: &[&[EnvSpec]] = &[array::ENV_SPECS, cd::ENV_SPECS];

/// `environments[name]`, with the interned name. (A linear scan: there are ~40 names.)
pub fn lookup(name: &str) -> Option<(&'static str, &'static EnvSpec)> {
    let mut found = None;
    for specs in ALL {
        for spec in specs.iter() {
            for n in spec.names {
                if *n == name {
                    found = Some((*n, spec)); // later definitions win
                }
            }
        }
    }
    found
}
