//! Resolve lexical bindings within an editor buffer. Only open source files need a syntax tree.
use super::*;
use std::{cell::RefCell, ops::Range};
use tree_sitter::{Node, Parser, Tree};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Local {
    pub span: Range<usize>,
    pub line: u32,
}

#[derive(Default)]
pub(super) struct Locals(RefCell<Option<(u64, Tree)>>);

impl Locals {
    pub(super) fn find(&self, doc: &Doc, word: &str, at: usize) -> Option<Local> {
        let text = doc.buf.text.as_str();
        if text.len() > 3_000_000 || !matches!(doc.lang, Lang::C | Lang::Rust) {
            return None;
        }
        let mut cached = self.0.borrow_mut();
        if cached.as_ref().is_none_or(|(ver, _)| *ver != doc.buf.ver) {
            let language = if doc.lang == Lang::Rust { tree_sitter_rust::LANGUAGE } else { tree_sitter_cpp::LANGUAGE };
            let mut parser = Parser::new();
            parser.set_language(&language.into()).ok()?;
            *cached = Some((doc.buf.ver, parser.parse(text, None)?));
        }
        let tree = &cached.as_ref()?.1;
        let node = tree.root_node().named_descendant_for_byte_range(at, at.checked_add(1)?)?;
        if !matches!(node.kind(), "identifier" | "shorthand_field_identifier") || text.get(node.byte_range())? != word {
            return None;
        }
        if node
            .parent()
            .is_some_and(|p| matches!(p.kind(), "qualified_identifier" | "scoped_identifier" | "labeled_statement" | "goto_statement"))
        {
            return None;
        }

        let query = Query {
            text,
            word,
            at,
            inactive: doc.inactive.ranges_for(doc.buf.ver),
        };
        let mut child = node;
        while let Some(scope) = child.parent() {
            let found = match scope.kind() {
                "compound_statement" | "block" | "case_statement" | "preproc_if" | "preproc_ifdef" | "preproc_else" | "preproc_elif" | "parameter_list" | "parameters" | "closure_parameters" => {
                    if scope.kind().starts_with("preproc_") && !is_local_scope(scope) {
                        break;
                    }
                    let with_parameters = matches!(scope.kind(), "parameter_list" | "parameters" | "closure_parameters");
                    query.siblings(child, with_parameters).or_else(|| if with_parameters { query.binding(child, true) } else { None })
                }
                // Declarations to the left of another declarator on the same statement also count.
                "declaration" | "let_declaration" | "parameter_declaration" | "optional_parameter_declaration" | "parameter" => {
                    if is_local_scope(scope) {
                        query.binding(scope, true)
                    } else {
                        None
                    }
                }
                "for_statement" | "for_range_loop" | "if_statement" | "switch_statement" | "while_statement" | "for_expression" => {
                    ["initializer", "condition"]
                        .iter()
                        .find_map(|name| scope.child_by_field_name(name).and_then(|n| query.binding(n, true)))
                        .or_else(|| {
                            // For a range loop, the binding is available in the body, while the range can use an outer binding.
                            if scope.kind() == "for_range_loop" && scope.child_by_field_name("body").is_some_and(|n| n.start_byte() <= at) {
                                scope.child_by_field_name("declarator").and_then(|n| query.declarator(n))
                            } else if scope.kind() == "for_expression" && scope.child_by_field_name("body").is_some_and(|n| n.start_byte() <= at) {
                                scope.child_by_field_name("pattern").and_then(|n| query.pattern(n))
                            } else {
                                None
                            }
                        })
                }
                "function_definition" | "function_item" | "lambda_expression" | "closure_expression" | "catch_clause" => query.parameters(scope),
                _ => None,
            };
            if let Some(node) = found {
                return Some(Local {
                    span: node.byte_range(),
                    line: node.start_position().row as u32,
                });
            }
            if matches!(scope.kind(), "function_definition" | "function_item") {
                break; // A local function item cannot bind variables of an enclosing function.
            }
            child = scope;
        }
        None
    }
}

fn is_local_scope(mut node: Node<'_>) -> bool {
    while let Some(p) = node.parent() {
        if matches!(
            p.kind(),
            "compound_statement" | "block" | "function_definition" | "function_item" | "lambda_expression" | "closure_expression"
        ) {
            return true;
        }
        if matches!(p.kind(), "field_declaration_list" | "translation_unit" | "source_file") {
            return false;
        }
        node = p;
    }
    false
}

struct Query<'a> {
    text: &'a str,
    word: &'a str,
    at: usize,
    inactive: &'a [(usize, usize)],
}

impl Query<'_> {
    fn accept<'a>(&self, n: Node<'a>) -> Option<Node<'a>> {
        let line = n.start_position().row;
        (n.start_byte() <= self.at && self.text.get(n.byte_range()) == Some(self.word) && !self.inactive.iter().any(|&(a, b)| (a..=b).contains(&line))).then_some(n)
    }

    fn declarator<'a>(&self, node: Node<'a>) -> Option<Node<'a>> {
        match node.kind() {
            "identifier" => self.accept(node),
            "structured_binding_declarator" => self.children_rev(node, |n| self.accept(n)),
            "reference_declarator" | "parenthesized_declarator" | "attributed_declarator" => self.children_rev(node, |n| self.declarator(n)),
            _ => node.child_by_field_name("declarator").and_then(|n| self.declarator(n)),
        }
    }

    fn pattern<'a>(&self, node: Node<'a>) -> Option<Node<'a>> {
        match node.kind() {
            "identifier" | "shorthand_field_identifier" => self.accept(node),
            "field_pattern" => node.child_by_field_name("pattern").or_else(|| node.child_by_field_name("name")).and_then(|n| self.pattern(n)),
            "tuple_pattern" | "slice_pattern" | "reference_pattern" | "mut_pattern" | "ref_pattern" | "captured_pattern" | "or_pattern" => self.children_rev(node, |n| self.pattern(n)),
            "struct_pattern" | "tuple_struct_pattern" => {
                let ty = node.child_by_field_name("type");
                self.children_rev(node, |n| if Some(n) == ty { None } else { self.pattern(n) })
            }
            _ => None,
        }
    }

    fn binding<'a>(&self, node: Node<'a>, direct: bool) -> Option<Node<'a>> {
        match node.kind() {
            "declaration" | "parameter_declaration" | "optional_parameter_declaration" | "condition_clause" => {
                let mut cursor = node.walk();
                node.children_by_field_name("declarator", &mut cursor)
                    .filter_map(|n| self.declarator(n))
                    .last()
                    .or_else(|| node.child_by_field_name("value").and_then(|n| self.binding(n, direct)))
            }
            "let_declaration" | "parameter" => {
                let pattern = node.child_by_field_name("pattern")?;
                (!direct || self.at < pattern.end_byte()).then(|| self.pattern(pattern)).flatten()
            }
            "preproc_if" | "preproc_ifdef" | "preproc_elif" | "preproc_else" => self.children_rev(node, |n| self.binding(n, false)),
            _ => None,
        }
    }

    fn siblings<'a>(&self, mut child: Node<'a>, parameters: bool) -> Option<Node<'a>> {
        while let Some(prev) = child.prev_named_sibling() {
            if let Some(n) = self.binding(prev, false).or_else(|| if parameters && prev.kind() == "identifier" { self.accept(prev) } else { None }) {
                return Some(n);
            }
            child = prev;
        }
        None
    }

    fn parameters<'a>(&self, scope: Node<'a>) -> Option<Node<'a>> {
        let params = scope.child_by_field_name("parameters").or_else(|| scope.child_by_field_name("parameter")).or_else(|| {
            let mut decl = scope.child_by_field_name("declarator")?;
            loop {
                if let Some(p) = decl.child_by_field_name("parameters") {
                    break Some(p);
                }
                decl = decl.child_by_field_name("declarator")?;
            }
        })?;
        self.children_rev(params, |n| {
            self.binding(n, false).or_else(|| if params.kind() == "closure_parameters" { self.pattern(n) } else { None })
        })
    }

    fn children_rev<'a>(&self, node: Node<'a>, mut f: impl FnMut(Node<'a>) -> Option<Node<'a>>) -> Option<Node<'a>> {
        (0..node.named_child_count()).rev().filter_map(|i| node.named_child(i as u32)).find_map(&mut f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(file: &str, text: &str) -> Doc {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join(file);
        std::fs::write(&source, text).unwrap();
        Doc::load(&source, dir.path()).unwrap()
    }

    #[test]
    fn c_parameters_shadowing_same_line_declarators_and_loop_scopes() {
        let text = "int value;\nvoid run(int gain) {\n  int value = gain, other = value;\n  { long value = 9; use(value); }\n  for (unsigned i = 0; i < 4; ++i) { use(i); }\n  use(value); use(i); use(packet.value);\n}\nvoid other(void) { use(gain); }";
        let doc = document("sample.c", text);
        let find = |anchor: &str, name: &str, offset: usize| doc.locals.find(&doc, name, text.find(anchor).unwrap() + offset);
        let value = text.find("value = gain").unwrap();
        assert_eq!(find("gain, other", "gain", 0).unwrap().line, 1);
        assert_eq!(find("value;\n  {", "value", 0).unwrap().span.start, value);
        assert_eq!(find("use(value); }", "value", 4).unwrap().span.start, text.find("value = 9").unwrap());
        assert_eq!(find("use(i); }", "i", 4).unwrap().span.start, text.find("i = 0").unwrap());
        assert_eq!(find("use(value); use(i)", "value", 4).unwrap().span.start, value);
        assert!(find("use(i); use(packet", "i", 4).is_none());
        assert!(find("packet.value", "value", 7).is_none());
        assert!(find("use(gain)", "gain", 4).is_none());
    }

    #[test]
    fn cpp_methods_and_rust_shadow_initializers_resolve_lexically() {
        let cpp = "namespace device { struct Board { int f(const int &arg) { const auto &copy = arg; return copy; } }; }";
        let doc = document("board.cpp", cpp);
        let arg = doc.locals.find(&doc, "arg", cpp.find("= arg").unwrap() + 2).unwrap();
        assert_eq!(arg.span.start, cpp.find("arg").unwrap());
        assert_eq!(doc.locals.find(&doc, "copy", cpp.rfind("copy").unwrap()).unwrap().span.start, cpp.find("copy").unwrap());
        let rust = "fn run(gain: i32) { let (sample, _) = (gain, 0); let sample = sample + 1; use_sample(sample); }";
        let doc = document("board.rs", rust);
        let binding = |b| doc.locals.find(&doc, "sample", b).map(|n| n.span.start);
        assert_eq!(binding(rust.find("= sample").unwrap() + 2), rust.find("sample"));
        assert_eq!(binding(rust.rfind("sample").unwrap()), rust.find("sample ="));
    }
}
