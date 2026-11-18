//! Code structure from tree-sitter: where each function, class and module starts and
//! ends, which names a line uses, and where a name is defined. It reads whole files, so
//! a diff's partial hunks never reach the parser.

use std::sync::LazyLock;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, StreamingIterator};

/// Larger files are skipped: a minified bundle would hold a worker for nothing.
const MAX_BYTES: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Function,
    Method,
    /// A class, struct, enum, union or type alias.
    Class,
    Interface,
    Module,
    Macro,
    Constant,
    /// A Rust impl block: it holds methods but defines no name of its own.
    Impl,
}

impl DefKind {
    fn from_capture(name: &str) -> Option<DefKind> {
        Some(match name {
            "definition.function" => DefKind::Function,
            "definition.method" => DefKind::Method,
            "definition.class" => DefKind::Class,
            "definition.interface" => DefKind::Interface,
            "definition.module" => DefKind::Module,
            "definition.macro" => DefKind::Macro,
            "definition.constant" => DefKind::Constant,
            "reference.implementation" => DefKind::Impl,
            _ => return None,
        })
    }

    /// A block other code sits inside, for "in Class > method".
    fn holds_code(self) -> bool {
        !matches!(self, DefKind::Constant | DefKind::Macro)
    }
}

/// One definition, with its lines from 1, as git and the diff number them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Def {
    pub kind: DefKind,
    pub name: String,
    pub first: usize,
    pub last: usize,
}

/// A file's definitions, in the order they start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outline {
    pub defs: Vec<Def>,
}

impl Outline {
    /// The blocks around a line, outermost first: "BaseApp > on_mount" is two.
    pub fn enclosing(&self, line: usize) -> Vec<&Def> {
        let mut around: Vec<&Def> = self
            .defs
            .iter()
            .filter(|def| def.kind.holds_code() && def.first <= line && line <= def.last)
            .collect();
        // Outermost first: earlier start, then the larger block.
        around.sort_by_key(|def| (def.first, usize::MAX - def.last));
        around
    }

    /// "BaseApp > on_mount", or None outside every block.
    pub fn place(&self, line: usize) -> Option<String> {
        let names: Vec<String> = self
            .enclosing(line)
            .iter()
            .map(|def| match def.kind {
                DefKind::Impl => format!("impl {}", def.name),
                _ => def.name.clone(),
            })
            .collect();
        (!names.is_empty()).then(|| names.join(" > "))
    }

    /// The lines where `name` is defined.
    pub fn definitions_of(&self, name: &str) -> Vec<usize> {
        self.defs
            .iter()
            .filter(|def| def.kind != DefKind::Impl && def.name == name)
            .map(|def| def.first)
            .collect()
    }
}

struct Grammar {
    extensions: &'static [&'static str],
    language: fn() -> Language,
    /// The tags queries, joined: TypeScript's builds on JavaScript's.
    tags: &'static [&'static str],
}

const GRAMMARS: [Grammar; 13] = [
    Grammar {
        extensions: &["py", "pyi"],
        language: || tree_sitter_python::LANGUAGE.into(),
        tags: &[tree_sitter_python::TAGS_QUERY],
    },
    Grammar {
        extensions: &["rs"],
        language: || tree_sitter_rust::LANGUAGE.into(),
        tags: &[tree_sitter_rust::TAGS_QUERY],
    },
    Grammar {
        extensions: &["js", "mjs", "cjs", "jsx"],
        language: || tree_sitter_javascript::LANGUAGE.into(),
        tags: &[tree_sitter_javascript::TAGS_QUERY],
    },
    Grammar {
        extensions: &["ts", "mts", "cts"],
        language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        tags: &[
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY,
        ],
    },
    Grammar {
        extensions: &["tsx"],
        language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
        tags: &[
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY,
        ],
    },
    Grammar {
        extensions: &["go"],
        language: || tree_sitter_go::LANGUAGE.into(),
        tags: &[tree_sitter_go::TAGS_QUERY],
    },
    Grammar {
        extensions: &["json"],
        language: || tree_sitter_json::LANGUAGE.into(),
        tags: &[],
    },
    Grammar {
        extensions: &["yaml", "yml"],
        language: || tree_sitter_yaml::LANGUAGE.into(),
        tags: &[],
    },
    Grammar {
        extensions: &["sh", "bash"],
        language: || tree_sitter_bash::LANGUAGE.into(),
        tags: &[],
    },
    Grammar {
        extensions: &["html", "htm"],
        language: || tree_sitter_html::LANGUAGE.into(),
        tags: &[],
    },
    Grammar {
        extensions: &["css"],
        language: || tree_sitter_css::LANGUAGE.into(),
        tags: &[],
    },
    Grammar {
        extensions: &["md", "markdown"],
        language: || tree_sitter_md::LANGUAGE.into(),
        tags: &[],
    },
    Grammar {
        extensions: &["toml"],
        language: || tree_sitter_toml_ng::LANGUAGE.into(),
        tags: &[],
    },
];

/// Each grammar's tags query, compiled once. None for a grammar without one.
static TAGS: LazyLock<Vec<Option<Query>>> = LazyLock::new(|| {
    GRAMMARS
        .iter()
        .map(|grammar| {
            if grammar.tags.is_empty() {
                return None;
            }
            let source = grammar.tags.join("\n");
            // The tests compile every query, so a failure here is a bug in a grammar
            // update; the file then gets no outline rather than a crash.
            Query::new(&(grammar.language)(), &source).ok()
        })
        .collect()
});

fn grammar_index(path: &str) -> Option<usize> {
    let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
    GRAMMARS
        .iter()
        .position(|grammar| grammar.extensions.contains(&extension.as_str()))
}

/// The extensions of the languages revu reads, for limiting a search to them.
pub fn code_extensions() -> Vec<&'static str> {
    GRAMMARS
        .iter()
        .zip(TAGS.iter())
        .filter(|(_, tags)| tags.is_some())
        .flat_map(|(grammar, _)| grammar.extensions.iter().copied())
        .collect()
}

fn parse(index: usize, code: &[u8]) -> Option<tree_sitter::Tree> {
    if code.len() > MAX_BYTES {
        return None;
    }
    let mut parser = Parser::new();
    parser.set_language(&(GRAMMARS[index].language)()).ok()?;
    parser.parse(code, None)
}

/// The file's definitions, or None for a language revu does not know or a file too
/// large to parse.
pub fn outline(path: &str, code: &[u8]) -> Option<Outline> {
    let index = grammar_index(path)?;
    let tree = parse(index, code)?;
    let Some(query) = &TAGS[index] else {
        return Some(Outline::default());
    };
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), code);
    let mut defs = Vec::new();
    while let Some(found) = matches.next() {
        let mut kind = None;
        let mut block = None;
        let mut name = None;
        for capture in found.captures {
            let capture_name = names[capture.index as usize];
            if capture_name == "name" {
                name = capture.node.utf8_text(code).ok();
            } else if let Some(found_kind) = DefKind::from_capture(capture_name) {
                kind = Some(found_kind);
                block = Some(capture.node);
            }
        }
        if let (Some(kind), Some(block), Some(name)) = (kind, block, name) {
            defs.push(Def {
                kind,
                name: name.to_string(),
                first: decorated_start(block) + 1,
                last: block.end_position().row + 1,
            });
        }
    }
    defs.sort_by_key(|def| (def.first, usize::MAX - def.last));
    // A method also matches the plain function pattern; keep one per block.
    defs.dedup_by(|later, earlier| later.first == earlier.first && later.name == earlier.name);
    Some(defs).map(|defs| Outline { defs })
}

/// The row a block's decorators or attributes start on: Python wraps a decorated
/// function in a parent node, while Rust's `#[test]` and TypeScript's decorators come
/// just before it. A change to `@property` is a change to the method under it.
fn decorated_start(block: Node) -> usize {
    if let Some(parent) = block.parent()
        && parent.kind() == "decorated_definition"
    {
        return parent.start_position().row;
    }
    let mut first = block;
    while let Some(before) = first.prev_named_sibling()
        && matches!(before.kind(), "attribute_item" | "decorator")
    {
        first = before;
    }
    first.start_position().row
}

/// The names a line uses or defines, for picking one to look up: calls and other
/// references the tags query knows first, then the line's other identifiers, each once.
pub fn names_on_line(path: &str, code: &[u8], line: usize) -> Vec<String> {
    let Some(index) = grammar_index(path) else {
        return Vec::new();
    };
    let Some(tree) = parse(index, code) else {
        return Vec::new();
    };
    let row = line.saturating_sub(1);
    let mut names = Vec::new();
    if let Some(query) = &TAGS[index] {
        let capture_names = query.capture_names();
        let mut cursor = QueryCursor::new();
        cursor.set_point_range(
            tree_sitter::Point { row, column: 0 }..tree_sitter::Point {
                row: row + 1,
                column: 0,
            },
        );
        let mut captures = cursor.captures(query, tree.root_node(), code);
        while let Some((found, index)) = captures.next() {
            let capture = found.captures[*index];
            let on_line = capture.node.start_position().row == row;
            if on_line && capture_names[capture.index as usize] == "name" {
                push_name(&mut names, capture.node, code);
            }
        }
    }
    collect_identifiers(tree.root_node(), row, code, &mut names);
    names
}

fn push_name(names: &mut Vec<String>, node: Node, code: &[u8]) {
    if let Ok(text) = node.utf8_text(code)
        && !names.iter().any(|known| known == text)
    {
        names.push(text.to_string());
    }
}

/// Every identifier-like leaf on the row, in the order they appear.
fn collect_identifiers(node: Node, row: usize, code: &[u8], names: &mut Vec<String>) {
    if node.start_position().row > row || node.end_position().row < row {
        return;
    }
    if node.child_count() == 0 {
        if node.kind().ends_with("identifier") && node.start_position().row == row {
            push_name(names, node, code);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_identifiers(child, row, code, names);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_starts_at_its_decorators_and_attributes() {
        let python = "class A:\n    x = 1\n\n    @property\n    def d(self):\n        return 1\n";
        let python = outline("a.py", python.as_bytes()).unwrap();
        assert_eq!(python.place(4).as_deref(), Some("A > d"), "@property");
        assert_eq!(python.place(2).as_deref(), Some("A"));
        let rust =
            "mod tests {\n    use super::*;\n\n    #[cfg(unix)]\n    #[test]\n    fn t() {}\n}\n";
        let rust = outline("a.rs", rust.as_bytes()).unwrap();
        assert_eq!(rust.place(4).as_deref(), Some("tests > t"), "#[cfg(unix)]");
        assert_eq!(rust.place(2).as_deref(), Some("tests"));
    }

    #[test]
    fn every_tags_query_compiles_against_its_grammar() {
        for (grammar, query) in GRAMMARS.iter().zip(TAGS.iter()) {
            assert_eq!(
                query.is_some(),
                !grammar.tags.is_empty(),
                "{:?}",
                grammar.extensions
            );
        }
    }

    #[test]
    fn every_language_parses() {
        let samples = [
            ("a.py", "x = 1\n"),
            ("a.rs", "fn a() {}\n"),
            ("a.js", "let a = 1;\n"),
            ("a.ts", "let a: number = 1;\n"),
            ("a.tsx", "const a = <div/>;\n"),
            ("a.go", "package a\n"),
            ("a.json", "{}\n"),
            ("a.yaml", "a: 1\n"),
            ("a.sh", "echo a\n"),
            ("a.html", "<p>a</p>\n"),
            ("a.css", "a { color: red; }\n"),
            ("a.md", "# a\n"),
            ("a.toml", "a = 1\n"),
        ];
        for (path, code) in samples {
            assert!(outline(path, code.as_bytes()).is_some(), "{path}");
        }
        assert!(outline("a.unknown", b"x").is_none());
        assert!(outline("Makefile", b"x").is_none());
    }

    const PYTHON: &str = "\
class BaseApp(ShellScreen):
    def on_mount(self):
        self._done = False

        def inner():
            return enumerate(x)

    def other(self):
        pass

def strip_prefix(a):
    return a
";

    #[test]
    fn a_line_knows_its_class_and_method() {
        let outline = outline("app.py", PYTHON.as_bytes()).unwrap();
        assert_eq!(outline.place(3).as_deref(), Some("BaseApp > on_mount"));
        assert_eq!(
            outline.place(6).as_deref(),
            Some("BaseApp > on_mount > inner")
        );
        assert_eq!(outline.place(9).as_deref(), Some("BaseApp > other"));
        assert_eq!(outline.place(12).as_deref(), Some("strip_prefix"));
        assert_eq!(outline.place(10), None, "between blocks");
        assert_eq!(
            outline.place(1).as_deref(),
            Some("BaseApp"),
            "the first line"
        );
    }

    #[test]
    fn rust_methods_sit_in_their_impl_and_nested_fns_in_theirs() {
        let code = "\
struct Foo;

impl Foo {
    fn bar(&self) {
        let x = 1;
    }
}

fn strip_prefix() {
    fn imp() {
        let y = 2;
    }
}
";
        let outline = outline("a.rs", code.as_bytes()).unwrap();
        assert_eq!(outline.place(5).as_deref(), Some("impl Foo > bar"));
        assert_eq!(outline.place(11).as_deref(), Some("strip_prefix > imp"));
        assert_eq!(outline.definitions_of("Foo"), vec![1], "not the impl");
    }

    #[test]
    fn a_definition_is_found_and_a_call_is_not() {
        let outline = outline("app.py", PYTHON.as_bytes()).unwrap();
        assert_eq!(outline.definitions_of("strip_prefix"), vec![11]);
        assert_eq!(outline.definitions_of("on_mount"), vec![2]);
        assert!(outline.definitions_of("enumerate").is_empty(), "a call");
    }

    #[test]
    fn typescript_finds_classes_methods_and_functions() {
        let code = "\
export class Store {
  load(id: string): void {
    fetchItem(id);
  }
}
function fetchItem(id: string) {}
";
        let outline = outline("a.ts", code.as_bytes()).unwrap();
        assert_eq!(outline.place(3).as_deref(), Some("Store > load"));
        assert_eq!(outline.definitions_of("fetchItem"), vec![6]);
    }

    #[test]
    fn a_line_offers_its_calls_first_then_its_other_names() {
        let names = names_on_line("app.py", PYTHON.as_bytes(), 6);
        assert_eq!(names, ["enumerate", "x"]);
        let names = names_on_line("app.py", PYTHON.as_bytes(), 3);
        assert_eq!(names, ["self", "_done"]);
    }

    #[test]
    fn a_file_too_large_is_skipped() {
        let code = "x = 1\n".repeat(MAX_BYTES / 6 + 1);
        assert!(outline("big.py", code.as_bytes()).is_none());
    }
}
