//! Port of KaTeX `src/functions/environment.ts`: `\begin{…}` / `\end{…}`.

use crate::define_function::{ArgType, EnvContext, FunctionContext, FunctionSpec};
use crate::environments;
use crate::functions;
use crate::parse_error::{Result, ierr, perr};
use crate::parse_node::{Mode, ParseNode, assert_character_group};
use crate::parser::{ArgSpec, Parser};

/// `parser.parseFunction()` with no arguments (KaTeX calls it to read the `\end{…}`).
fn parse_function(parser: &mut Parser) -> Result<Option<ParseNode>> {
    let token = parser.fetch()?.clone();
    let Some((func, func_data)) = functions::lookup(&token.text) else {
        return Ok(None);
    };
    parser.consume();
    if parser.mode == Mode::Text && !func_data.allowed_in_text {
        return perr(
            format!("Can't use function '{}' in text mode", func),
            token.loc.as_ref(),
        );
    } else if parser.mode == Mode::Math && !func_data.allowed_in_math {
        return perr(
            format!("Can't use function '{}' in math mode", func),
            token.loc.as_ref(),
        );
    }
    let spec = ArgSpec {
        num_args: func_data.num_args,
        num_optional_args: func_data.num_optional_args,
        arg_types: func_data.arg_types,
        primitive: func_data.primitive,
        is_sqrt: func_data.node_type == "sqrt",
    };
    let (args, opt_args) = parser.parse_arguments(func, &spec)?;
    Ok(Some(parser.call_function(
        func,
        args,
        opt_args,
        Some(token),
        None,
    )?))
}

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let name_group = args.into_iter().next().unwrap();
    let ParseNode::OrdGroup(g) = &name_group else {
        return perr("Invalid environment name", name_group.loc());
    };
    let env_name = assert_character_group(
        g,
        "Environment name should contain only text characters and spaces",
        true,
    )?;
    if ctx.func_name == "\\begin" {
        let Some((env_name_static, env)) = environments::lookup(&env_name) else {
            return perr(format!("No such environment: {}", env_name), g.loc.as_ref());
        };
        let spec = ArgSpec {
            num_args: env.num_args,
            num_optional_args: 0,
            arg_types: env.arg_types,
            primitive: false,
            is_sqrt: env.node_type == "sqrt",
        };
        let parser = &mut *ctx.parser;
        let (args, opt_args) =
            parser.parse_arguments(&format!("\\begin{{{}}}", env_name), &spec)?;
        let mut env_ctx = EnvContext {
            mode: parser.mode,
            env_name: env_name_static,
            parser: &mut *parser,
        };
        let result = (env.handler)(&mut env_ctx, args, opt_args)?;
        parser.expect("\\end", false)?;
        let end_name_token = parser.next_token.clone();
        let end = parse_function(parser)?;
        let end_name = match &end {
            Some(ParseNode::Environment { name, .. }) => name.clone(),
            Some(other) => {
                return ierr(format!(
                    "Expected node of type environment, but got node of type {}",
                    other.type_name()
                ));
            }
            None => return ierr("Expected node of type environment, but got null"),
        };
        if end_name != env_name {
            return perr(
                format!(
                    "Mismatch: \\begin{{{}}} matched by \\end{{{}}}",
                    env_name, end_name
                ),
                end_name_token.as_ref().and_then(|t| t.loc.as_ref()),
            );
        }
        return Ok(result);
    }
    Ok(ParseNode::Environment {
        mode: ctx.parser.mode,
        name: env_name,
        name_group: Box::new(name_group),
    })
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "environment",
    names: &["\\begin", "\\end"],
    num_args: 1,
    arg_types: &[ArgType::Text],
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];
