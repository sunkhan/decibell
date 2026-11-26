//! Port of KaTeX `src/buildHTML.ts`, including the `_htmlGroupBuilders` dispatch (by node
//! type, to the module that ports the KaTeX file defining that builder).

use crate::build_common::{make_glue, make_span, make_span_s, try_combine_chars};
use crate::dom_tree::{CssProp, CssStyle, HtmlNode, Span};
use crate::environments;
use crate::functions::*;
use crate::options::Options;
use crate::parse_error::{Error, Result, perr};
use crate::parse_node::ParseNode;
use crate::spacing_data::{spacing, tight_spacing};
use crate::style::Style;
use crate::units::make_em;

/// What a builder reports for a node type whose port is missing: never a silent fallback.
pub fn not_ported<T>(group: &ParseNode) -> Result<T> {
    Err(Error::Internal(format!(
        "decibell-tex: KaTeX builder for '{}' is not ported",
        group.type_name()
    )))
}

fn bin_left_canceller(t: &str) -> bool {
    matches!(t, "leftmost" | "mbin" | "mopen" | "mrel" | "mop" | "mpunct")
}

fn bin_right_canceller(t: &str) -> bool {
    matches!(t, "rightmost" | "mrel" | "mclose" | "mpunct")
}

/// `DomEnum[className]`.
fn dom_type(class: &str) -> Option<&'static str> {
    Some(match class {
        "mord" => "mord",
        "mop" => "mop",
        "mbin" => "mbin",
        "mrel" => "mrel",
        "mopen" => "mopen",
        "mclose" => "mclose",
        "mpunct" => "mpunct",
        "minner" => "minner",
        _ => return None,
    })
}

/// `isRealGroup`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RealGroup {
    No,
    Yes,
    Root,
}

impl From<bool> for RealGroup {
    fn from(b: bool) -> RealGroup {
        if b { RealGroup::Yes } else { RealGroup::No }
    }
}

/// `checkPartialGroup(node)`.
fn partial_group_children(node: &HtmlNode) -> Option<&Vec<HtmlNode>> {
    match node {
        HtmlNode::Fragment(f) => Some(&f.children),
        HtmlNode::Anchor(a) => Some(&a.children),
        HtmlNode::Span(s) if s.has_class("enclosing") => Some(&s.children),
        _ => None,
    }
}

fn partial_group_children_mut(node: &mut HtmlNode) -> Option<&mut Vec<HtmlNode>> {
    match node {
        HtmlNode::Fragment(f) => Some(&mut f.children),
        HtmlNode::Anchor(a) => Some(&mut a.children),
        HtmlNode::Span(s) if s.has_class("enclosing") => Some(&mut s.children),
        _ => None,
    }
}

fn node_at<'a>(nodes: &'a [HtmlNode], path: &[usize]) -> &'a HtmlNode {
    let mut n = &nodes[path[0]];
    for &i in &path[1..] {
        n = &partial_group_children(n).unwrap()[i];
    }
    n
}

fn node_at_mut<'a>(nodes: &'a mut [HtmlNode], path: &[usize]) -> &'a mut HtmlNode {
    let mut n = &mut nodes[path[0]];
    for &i in &path[1..] {
        n = &mut partial_group_children_mut(n).unwrap()[i];
    }
    n
}

fn array_at_mut<'a>(nodes: &'a mut Vec<HtmlNode>, array_path: &[usize]) -> &'a mut Vec<HtmlNode> {
    let mut arr = nodes;
    for &i in array_path {
        arr = partial_group_children_mut(&mut arr[i]).unwrap();
    }
    arr
}

/// One step of `traverseNonSpaceNodes`: a visited (non-partial-group) node.
struct Visit {
    /// `None` for the dummy "next" node appended at the top level.
    path: Option<Vec<usize>>,
    nonspace: bool,
    newline: bool,
}

fn collect_visits(nodes: &[HtmlNode], prefix: &mut Vec<usize>, out: &mut Vec<Visit>) {
    for (i, node) in nodes.iter().enumerate() {
        prefix.push(i);
        if let Some(children) = partial_group_children(node) {
            collect_visits(children, prefix, out);
        } else {
            out.push(Visit {
                path: Some(prefix.clone()),
                nonspace: !node.has_class("mspace"),
                newline: node.has_class("katex-newline"),
            });
        }
        prefix.pop();
    }
}

/// The node `prev` refers to in `traverseNonSpaceNodes`.
#[derive(Clone)]
enum Prev {
    Dummy(&'static str),
    Node(Vec<usize>),
}

/// `buildExpression(expression, options, isRealGroup, surrounding)`.
pub fn build_expression(
    expression: &[ParseNode],
    options: &Options,
    is_real_group: RealGroup,
    surrounding: (Option<&'static str>, Option<&'static str>),
) -> Result<Vec<HtmlNode>> {
    let mut groups: Vec<HtmlNode> = Vec::with_capacity(expression.len());
    for e in expression {
        let output = build_group(Some(e), options, None)?;
        if let HtmlNode::Fragment(f) = output {
            groups.extend(f.children);
        } else {
            groups.push(output);
        }
    }
    try_combine_chars(&mut groups);
    if is_real_group == RealGroup::No {
        return Ok(groups);
    }
    let mut glue_options = options.clone();
    if expression.len() == 1 {
        match &expression[0] {
            ParseNode::Sizing { size, .. } => glue_options = options.having_size(*size),
            ParseNode::Styling { style, .. } => {
                glue_options = options.having_style(Style::from_style_str(*style))
            }
            _ => {}
        }
    }
    let is_root = is_real_group == RealGroup::Root;
    let tight = options.style.is_tight();
    let prev_dummy = surrounding.0.unwrap_or("leftmost");
    let next_dummy = surrounding.1.unwrap_or("rightmost");

    let mut visits = Vec::new();
    collect_visits(&groups, &mut Vec::new(), &mut visits);
    visits.push(Visit {
        path: None,
        nonspace: true,
        newline: false,
    });

    // Pass 1: bin cancellation (mutates classes[0]).
    {
        let mut prev = Prev::Dummy(prev_dummy);
        let mut next_class = next_dummy;
        for v in &visits {
            if v.nonspace {
                let prev_type: &str = match &prev {
                    Prev::Dummy(c) => c,
                    Prev::Node(p) => node_at(&groups, p).first_class(),
                };
                let ty: &str = match &v.path {
                    None => next_class,
                    Some(p) => node_at(&groups, p).first_class(),
                };
                if prev_type == "mbin" && bin_right_canceller(ty) {
                    match &mut prev {
                        Prev::Dummy(c) => *c = "mord",
                        Prev::Node(p) => {
                            if let Some(cl) = node_at_mut(&mut groups, p).classes_mut() {
                                cl[0] = "mord";
                            }
                        }
                    }
                } else if ty == "mbin" && bin_left_canceller(prev_type) {
                    match &v.path {
                        None => next_class = "mord",
                        Some(p) => {
                            if let Some(cl) = node_at_mut(&mut groups, p).classes_mut() {
                                cl[0] = "mord";
                            }
                        }
                    }
                }
                prev = match &v.path {
                    None => Prev::Dummy(next_class),
                    Some(p) => Prev::Node(p.clone()),
                };
            } else if is_root && v.newline {
                prev = Prev::Dummy("leftmost");
            }
        }
    }

    // Pass 2: inter-atom glue, inserted after the last visited node.
    let mut inserts: Vec<(Vec<usize>, usize, HtmlNode)> = Vec::new();
    {
        let mut prev = Prev::Dummy(prev_dummy);
        let mut insert_after: Option<(Vec<usize>, usize)> = None;
        for v in &visits {
            if v.nonspace {
                let prev_class: &str = match &prev {
                    Prev::Dummy(c) => c,
                    Prev::Node(p) => node_at(&groups, p).first_class(),
                };
                let (class, mtight) = match &v.path {
                    None => (next_dummy, tight),
                    Some(p) => {
                        let n = node_at(&groups, p);
                        (n.first_class(), n.has_class("mtight"))
                    }
                };
                let space = match (dom_type(prev_class), dom_type(class)) {
                    (Some(a), Some(b)) => {
                        if mtight {
                            tight_spacing(a, b)
                        } else {
                            spacing(a, b)
                        }
                    }
                    _ => None,
                };
                if let Some(space) = space {
                    let glue = make_glue(&space, &glue_options)?;
                    match &insert_after {
                        Some((arr, idx)) => inserts.push((arr.clone(), idx + 1, glue)),
                        None => {
                            let arr = match &v.path {
                                Some(p) => p[..p.len() - 1].to_vec(),
                                None => vec![],
                            };
                            inserts.push((arr, 0, glue));
                        }
                    }
                }
                prev = match &v.path {
                    None => Prev::Dummy(next_dummy),
                    Some(p) => Prev::Node(p.clone()),
                };
            } else if is_root && v.newline {
                prev = Prev::Dummy("leftmost");
            }
            if let Some(p) = &v.path {
                insert_after = Some((p[..p.len() - 1].to_vec(), p[p.len() - 1]));
            }
        }
    }
    for (arr, idx, glue) in inserts.into_iter().rev() {
        let a = array_at_mut(&mut groups, &arr);
        a.insert(idx.min(a.len()), glue);
    }
    Ok(groups)
}

/// `getOutermostNode(node, side)`.
fn outermost(node: &HtmlNode, right: bool) -> &HtmlNode {
    if let Some(children) = partial_group_children(node)
        && !children.is_empty()
    {
        return outermost(
            if right {
                children.last().unwrap()
            } else {
                &children[0]
            },
            right,
        );
    }
    node
}

/// `getTypeOfDomTree(node, side)`; `side`: None, Some(false) = left, Some(true) = right.
pub fn get_type_of_dom_tree(node: Option<&HtmlNode>, side: Option<bool>) -> Option<&'static str> {
    let node = node?;
    let node = match side {
        Some(right) => outermost(node, right),
        None => node,
    };
    dom_type(node.first_class())
}

/// `makeNullDelimiter(options, classes)`.
pub fn make_null_delimiter(options: &Options, classes: Vec<&'static str>) -> HtmlNode {
    let mut c = classes;
    c.push("nulldelimiter");
    c.extend(options.base_sizing_classes());
    make_span(c, vec![], None, CssStyle::new())
}

/// The `_htmlGroupBuilders` dispatch.
fn dispatch(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    use ParseNode as P;
    match group {
        P::Array(_) => environments::array::html_builder(group, options),
        P::CdLabel { .. } => environments::cd::html_builder_cdlabel(group, options),
        P::CdLabelParent { .. } => environments::cd::html_builder_cdlabelparent(group, options),
        P::Color { .. } => color::html_builder(group, options),
        P::Op(_) => op::html_builder(group, options),
        P::OrdGroup(_) => ordgroup::html_builder(group, options),
        P::Styling { .. } => styling::html_builder(group, options),
        P::SupSub(_) => supsub::html_builder(group, options),
        P::Text { .. } => text::html_builder(group, options),
        P::Verb { .. } => verb::html_builder(group, options),
        P::Atom(_) => symbols_op::html_builder(group, options),
        P::MathOrd(_) => symbols_ord::html_builder_mathord(group, options),
        P::TextOrd(_) => symbols_ord::html_builder_textord(group, options),
        P::Spacing(_) => symbols_spacing::html_builder(group, options),
        P::Accent(_) => accent::html_builder(group, options),
        P::AccentUnder(_) => accentunder::html_builder(group, options),
        P::Cr { .. } => cr::html_builder(group, options),
        P::DelimSizing { .. } => delimsizing::html_builder_delimsizing(group, options),
        P::Enclose(_) => enclose::html_builder(group, options),
        P::Font { .. } => font::html_builder(group, options),
        P::GenFrac(_) => genfrac::html_builder(group, options),
        P::HBox { .. } => hbox::html_builder(group, options),
        P::HorizBrace { .. } => horiz_brace::html_builder(group, options),
        P::Href { .. } => href::html_builder(group, options),
        P::Html { .. } => html::html_builder(group, options),
        P::HtmlMathml { .. } => htmlmathml::html_builder(group, options),
        P::IncludeGraphics(_) => includegraphics::html_builder(group, options),
        P::Kern { .. } => kern::html_builder(group, options),
        P::Lap { .. } => lap::html_builder(group, options),
        P::LeftRight(_) => delimsizing::html_builder_leftright(group, options),
        P::MathChoice(_) => mathchoice::html_builder(group, options),
        P::Middle { .. } => delimsizing::html_builder_middle(group, options),
        P::MClass { .. } => mclass::html_builder(group, options),
        P::OperatorName { .. } => operatorname::html_builder(group, options),
        P::Overline { .. } => overline::html_builder(group, options),
        P::Underline { .. } => underline::html_builder(group, options),
        P::Phantom { .. } => phantom::html_builder_phantom(group, options),
        P::VPhantom { .. } => phantom::html_builder_vphantom(group, options),
        P::Pmb { .. } => pmb::html_builder(group, options),
        P::RaiseBox { .. } => raisebox::html_builder(group, options),
        P::Rule { .. } => rule::html_builder(group, options),
        P::Sizing { .. } => sizing::html_builder(group, options),
        P::Smash { .. } => smash::html_builder(group, options),
        P::Sqrt { .. } => sqrt::html_builder(group, options),
        P::VCenter { .. } => vcenter::html_builder(group, options),
        P::XArrow { .. } => arrow::html_builder(group, options),
        // No HTML builder in KaTeX.
        P::ColorToken { .. }
        | P::Raw { .. }
        | P::Size { .. }
        | P::Tag { .. }
        | P::Url { .. }
        | P::AccentToken(_)
        | P::OpToken(_)
        | P::Environment { .. }
        | P::Infix { .. }
        | P::Internal { .. }
        | P::LeftRightRight { .. } => perr(
            format!("Got group of unknown type: '{}'", group.type_name()),
            None,
        ),
    }
}

/// `buildGroup(group, options, baseOptions)`.
pub fn build_group(
    group: Option<&ParseNode>,
    options: &Options,
    base_options: Option<&Options>,
) -> Result<HtmlNode> {
    let Some(group) = group else {
        return Ok(make_span(vec![], vec![], None, CssStyle::new()));
    };
    let _depth = crate::parse_error::DepthGuard::enter()?;
    let mut node = dispatch(group, options)?;
    if let Some(base) = base_options
        && options.size != base.size
    {
        let h = node.height();
        let d = node.depth();
        let mut wrapped = make_span_s(
            options.sizing_classes(base),
            vec![node],
            Some(options),
            CssStyle::new(),
        );
        let multiplier = options.size_multiplier / base.size_multiplier;
        wrapped.height = h * multiplier;
        wrapped.depth = d * multiplier;
        node = HtmlNode::span(wrapped);
    }
    Ok(node)
}

/// `buildHTMLUnbreakable(children, options)`.
fn build_html_unbreakable(children: Vec<HtmlNode>, options: &Options) -> Span {
    let mut body = make_span_s(vec!["katex-base"], children, Some(options), CssStyle::new());
    let mut strut = make_span_s(vec!["katex-strut"], vec![], None, CssStyle::new());
    strut
        .style
        .set(CssProp::Height, make_em(body.height + body.depth));
    if body.depth != 0.0 {
        strut
            .style
            .set(CssProp::VerticalAlign, make_em(-body.depth));
    }
    body.children.insert(0, HtmlNode::span(strut));
    body
}

/// `buildHTML(tree, options)`.
pub fn build_html(tree: &[ParseNode], options: &Options) -> Result<Span> {
    let (tree, tag): (&[ParseNode], Option<&[ParseNode]>) = match tree {
        [ParseNode::Tag { body, tag, .. }] => (body, Some(tag)),
        _ => (tree, None),
    };
    let mut expression = build_expression(tree, options, RealGroup::Root, (None, None))?;
    let mut eqn_num = None;
    if expression.len() == 2 && expression[1].has_class("katex-tag") {
        eqn_num = expression.pop();
    }
    let mut children: Vec<HtmlNode> = Vec::new();
    let mut parts: Vec<HtmlNode> = Vec::new();
    let mut it = expression.into_iter().peekable();
    while let Some(e) = it.next() {
        let breaks = e.has_class("mbin") || e.has_class("mrel") || e.has_class("allowbreak");
        let newline = e.has_class("katex-newline");
        parts.push(e);
        if breaks {
            let mut nobreak = false;
            while let Some(n) = it.peek() {
                if n.has_class("mspace") && !n.has_class("katex-newline") {
                    let n = it.next().unwrap();
                    if n.has_class("nobreak") {
                        nobreak = true;
                    }
                    parts.push(n);
                } else {
                    break;
                }
            }
            if !nobreak {
                children.push(HtmlNode::span(build_html_unbreakable(
                    std::mem::take(&mut parts),
                    options,
                )));
            }
        } else if newline {
            let nl = parts.pop().unwrap();
            if !parts.is_empty() {
                children.push(HtmlNode::span(build_html_unbreakable(
                    std::mem::take(&mut parts),
                    options,
                )));
            }
            children.push(nl);
        }
    }
    if !parts.is_empty() {
        children.push(HtmlNode::span(build_html_unbreakable(parts, options)));
    }
    let mut has_tag_child = false;
    if let Some(tag) = tag {
        let mut tag_child = build_html_unbreakable(
            build_expression(tag, options, RealGroup::Yes, (None, None))?,
            options,
        );
        tag_child.classes = vec!["katex-tag"];
        children.push(HtmlNode::span(tag_child));
        has_tag_child = true;
    } else if let Some(e) = eqn_num {
        children.push(e);
    }
    let mut html = make_span_s(vec!["katex-html"], children, None, CssStyle::new());
    html.set_attribute("aria-hidden", "true");
    if has_tag_child {
        let (h, d) = (html.height, html.depth);
        if let Some(HtmlNode::Span(tag_child)) = html.children.last_mut()
            && let Some(HtmlNode::Span(strut)) = tag_child.children.first_mut()
        {
            strut.style.set(CssProp::Height, make_em(h + d));
            if d != 0.0 {
                strut.style.set(CssProp::VerticalAlign, make_em(-d));
            }
        }
    }
    Ok(html)
}
