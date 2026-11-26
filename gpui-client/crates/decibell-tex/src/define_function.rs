//! Port of KaTeX `src/defineFunction.ts` and `src/defineEnvironment.ts`.
//!
//! A KaTeX `defineFunction({...})` call becomes one `FunctionSpec` in the `SPECS` slice of the
//! matching `functions/*.rs` module; its `htmlBuilder` is the module's builder, dispatched by
//! node type in `build_html::build_group`. The name → spec lookup is the sorted static
//! table in `functions/table.rs` (kept current by a unit test).

use crate::parse_error::Result;
use crate::parse_node::{Mode, ParseNode};
use crate::parser::Parser;
use crate::token::Token;

/// `ArgType` (types/index.ts).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgType {
    Color,
    Size,
    Url,
    Raw,
    Original,
    HBox,
    Primitive,
    Math,
    Text,
}

/// `BreakToken`.
pub type BreakToken = &'static str;

/// `FunctionContext`.
pub struct FunctionContext<'p, 's> {
    pub func_name: &'static str,
    pub parser: &'p mut Parser<'s>,
    pub token: Option<Token>,
    pub break_on_token_text: Option<BreakToken>,
}

/// `FunctionHandler`: `args` are the mandatory arguments, `opt_args` the optional ones.
pub type FunctionHandler =
    fn(&mut FunctionContext, Vec<ParseNode>, Vec<Option<ParseNode>>) -> Result<ParseNode>;

/// `FunctionSpec` + its `names`.
pub struct FunctionSpec {
    /// The node type the handler produces (KaTeX's `type`; Parser tests it for `"sqrt"`).
    pub node_type: &'static str,
    pub names: &'static [&'static str],
    pub num_args: usize,
    /// `argTypes`; an index past the end is KaTeX's `undefined`.
    pub arg_types: &'static [ArgType],
    pub allowed_in_argument: bool,
    pub allowed_in_text: bool,
    /// KaTeX only refuses when `allowedInMath === false`, so this defaults to true.
    pub allowed_in_math: bool,
    pub num_optional_args: usize,
    pub infix: bool,
    pub primitive: bool,
    pub handler: Option<FunctionHandler>,
}

impl FunctionSpec {
    /// The defaults of an object literal that sets only `type`, `names`, `numArgs`, `handler`.
    pub const DEFAULT: FunctionSpec = FunctionSpec {
        node_type: "",
        names: &[],
        num_args: 0,
        arg_types: &[],
        allowed_in_argument: false,
        allowed_in_text: false,
        allowed_in_math: true,
        num_optional_args: 0,
        infix: false,
        primitive: false,
        handler: None,
    };
}

/// `EnvContext`.
pub struct EnvContext<'p, 's> {
    pub mode: Mode,
    pub env_name: &'static str,
    pub parser: &'p mut Parser<'s>,
}

pub type EnvHandler =
    fn(&mut EnvContext, Vec<ParseNode>, Vec<Option<ParseNode>>) -> Result<ParseNode>;

/// `EnvSpec` + its `names`.
pub struct EnvSpec {
    pub node_type: &'static str,
    pub names: &'static [&'static str],
    pub num_args: usize,
    pub arg_types: &'static [ArgType],
    pub handler: EnvHandler,
}
