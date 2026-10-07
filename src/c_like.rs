//! Definitions in C and C++ sources, read line by line like the other
//! languages but with enough lookahead for the common layouts: a function's
//! return type on the same or the previous line, its parameters spanning
//! several lines, and its opening brace on the same or the next line.

pub(crate) const EXTENSIONS: &[&str] = &["c", "h", "cc", "cpp", "hpp"];

/// Lines a function's parameter list may span before the search gives up.
const MAX_SIGNATURE_LINES: usize = 32;

const NOT_FUNCTIONS: &[&str] = &[
    "if", "for", "while", "switch", "return", "sizeof", "defined",
];

#[derive(Default)]
pub(crate) struct Definitions {
    /// Functions and types, as `kind name`.
    pub(crate) definitions: Vec<String>,
    /// `#define` names. Kept apart so a generated register header cannot
    /// crowd the functions and types out of a file's definition list.
    pub(crate) macros: Vec<String>,
    /// Names for search and reference lookup: functions, types and
    /// function-like macros. Constants stay out: a register header defines
    /// tens of thousands, which cost 5x query memory and ranked no better
    /// (docs/experiments.md).
    pub(crate) symbols: Vec<String>,
    /// Quoted `#include` targets, relative to the including file.
    pub(crate) imports: Vec<String>,
}

impl Definitions {
    fn add(&mut self, kind: &str, display: &str, name: &str) {
        if is_identifier(name) {
            self.definitions.push(format!("{kind} {display}"));
            self.symbols.push(name.to_owned());
        }
    }
}

pub(crate) fn parse(text: &str) -> Definitions {
    let mut in_comment = false;
    let lines: Vec<&str> = text
        .lines()
        .map(|line| code_part(line, &mut in_comment))
        .collect();
    let mut found = Definitions::default();
    let mut continued = false;
    let mut typedef_open = false;
    let mut include_guard: Option<&str> = None;

    for (index, line) in lines.iter().enumerate() {
        let was_continued = continued;
        continued = line.trim_end().ends_with('\\');
        let trimmed = line.trim();
        if was_continued {
            continue;
        }
        if let Some(directive) = trimmed.strip_prefix('#') {
            let directive = directive.trim_start();
            if let Some(rest) = directive.strip_prefix("ifndef") {
                include_guard = Some(leading_identifier(rest.trim_start()));
            } else if let Some(rest) = directive.strip_prefix("define") {
                let rest = rest.trim_start();
                let name = leading_identifier(rest);
                let guard = include_guard == Some(name) && rest[name.len()..].trim().is_empty();
                if !guard && is_identifier(name) {
                    found.macros.push(format!("define {name}"));
                    if rest[name.len()..].starts_with('(') {
                        found.symbols.push(name.to_owned());
                    }
                }
            } else if let Some(rest) = directive.strip_prefix("include") {
                if let Some(target) = rest
                    .trim()
                    .strip_prefix('"')
                    .and_then(|rest| rest.split('"').next())
                {
                    if !target.is_empty() {
                        found.imports.push(if target.starts_with('.') {
                            target.to_owned()
                        } else {
                            format!("./{target}")
                        });
                    }
                }
            }
            continue;
        }
        if line.starts_with([' ', '\t']) || trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('}') {
            if typedef_open {
                typedef_open = false;
                if let Some(name) = typedef_name(rest) {
                    found.add("typedef", name, name);
                }
            }
            continue;
        }

        let (typedef, body) = match trimmed.strip_prefix("typedef ") {
            Some(rest) => (true, rest.trim_start()),
            None => (false, trimmed),
        };
        if let Some((kind, name)) = type_definition(body, next_code_line(&lines, index + 1)) {
            if !name.is_empty() {
                found.add(kind, name, name);
            }
            if typedef {
                match trimmed.rfind('}') {
                    Some(close) => {
                        if let Some(name) = typedef_name(&trimmed[close + 1..]) {
                            found.add("typedef", name, name);
                        }
                    }
                    None => typedef_open = true,
                }
            }
            continue;
        }
        if typedef {
            if trimmed.ends_with(';') {
                if let Some(name) = typedef_name(body) {
                    found.add("typedef", name, name);
                }
            }
            continue;
        }
        if let Some((display, name)) = function_definition(&lines, index) {
            found.add("fn", display, name);
        }
    }
    found
}

/// The code on a line, without comments: empty when the line starts inside
/// a block comment, cut where a block comment opens without closing or a
/// line comment starts. Comment markers inside string literals are skipped.
fn code_part<'a>(line: &'a str, in_comment: &mut bool) -> &'a str {
    if *in_comment {
        // Code after a comment that closes mid-line is rare; it is dropped
        // rather than parsed without its indentation.
        *in_comment = !line.contains("*/");
        return "";
    }
    let mut search = 0;
    while let Some(at) = line[search..].find('/').map(|at| at + search) {
        let quoted = line[..at].matches('"').count() % 2 == 1;
        let next = line[at + 1..].chars().next();
        if quoted || !matches!(next, Some('*' | '/')) {
            search = at + 1;
            continue;
        }
        if next == Some('/') {
            return &line[..at];
        }
        match line[at + 2..].find("*/") {
            Some(end) => search = at + 2 + end + 2,
            None => {
                *in_comment = true;
                return &line[..at];
            }
        }
    }
    line
}

/// `struct|union|enum|class NAME` opening a body on this line or the next.
/// The name is empty for an anonymous type.
fn type_definition<'a>(body: &'a str, next: &str) -> Option<(&'static str, &'a str)> {
    for kind in ["struct", "union", "enum", "class"] {
        let Some(rest) = body
            .strip_prefix(kind)
            .filter(|rest| rest.is_empty() || rest.starts_with([' ', '{']))
        else {
            continue;
        };
        let mut rest = rest.trim_start();
        if kind == "enum" {
            rest = rest
                .strip_prefix("class ")
                .or_else(|| rest.strip_prefix("struct "))
                .unwrap_or(rest)
                .trim_start();
        }
        rest = skip_attributes(rest);
        let mut name = leading_identifier(rest);
        let mut after = rest[name.len()..].trim_start();
        // A nested type defined out of line: `struct Optimizer::Impl {`.
        while let Some(inner) = after.strip_prefix("::") {
            name = leading_identifier(inner);
            after = inner[name.len()..].trim_start();
        }
        if after.starts_with('<') {
            after = skip_balanced(after, '<', '>').trim_start();
        }
        after = after.strip_prefix("final").map_or(after, str::trim_start);
        let opens = after.starts_with('{')
            || (after.starts_with(':') && !after.starts_with("::") && !after.contains(';'))
            || (after.is_empty() && next.starts_with('{'));
        return opens.then_some((kind, name));
    }
    None
}

/// A function whose signature starts on `lines[index]` and which has a
/// body: `(display, name)`, where display keeps a C++ `Class::` qualifier.
fn function_definition<'a>(lines: &[&'a str], index: usize) -> Option<(&'a str, &'a str)> {
    let first = lines[index].trim();
    let open = parameters_open(first)?;
    let head = first[..open].trim_end();
    if head.is_empty() || head.contains('=') {
        return None;
    }
    let start = after_last(head, |character| {
        !(character.is_ascii_alphanumeric()
            || character == '_'
            || character == ':'
            || character == '~')
    });
    let qualified = head[start..].trim_start_matches(':');
    let simple = qualified
        .rsplit("::")
        .next()
        .unwrap_or("")
        .trim_start_matches('~');
    if simple.is_empty() || NOT_FUNCTIONS.contains(&simple) {
        return None;
    }
    // A bare `name(` needs its return type on the line above (GNU layout);
    // otherwise it is an annotation such as `__acquires(lock)` or a call.
    if start == 0 && !qualified.contains("::") && !is_macro_name(simple) {
        let above = index.checked_sub(1).map_or("", |above| lines[above]);
        let typed = without_trailing_attributes(above).ends_with(|character: char| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '*' | '&' | '>')
        });
        if !typed || above.starts_with([' ', '\t', '#']) {
            return None;
        }
    }

    // Walk the parameter list to its closing parenthesis, then decide from
    // what follows: a body, or a declaration, initializer or call.
    let mut depth = 0usize;
    let mut line_index = index;
    let mut text = &first[open..];
    let after_close = 'scan: loop {
        for (at, character) in text.char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break 'scan &text[at + 1..];
                    }
                }
                _ => {}
            }
        }
        line_index += 1;
        if line_index >= lines.len() || line_index > index + MAX_SIGNATURE_LINES {
            return None;
        }
        text = lines[line_index].trim();
    };
    let rest = after_close.trim();
    let has_body = match rest.find('{') {
        Some(brace) => !rest[..brace].contains([';', '=']),
        // A constructor's initializer list: `Foo::Foo(int x) :`.
        None if rest.starts_with(':') && !rest.starts_with("::") => true,
        None if rest.contains([';', '=']) || rest.ends_with(',') => false,
        None => {
            // Lock annotations may sit between the signature and the body:
            // `__releases(lock)` / `__must_hold(&lock)` on their own lines.
            let next = lines[line_index + 1..]
                .iter()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .take(5)
                .find(|line| !is_annotation(line))
                .unwrap_or("");
            let next = strip_qualifiers(next);
            next.starts_with('{') || (next.starts_with(':') && !next.starts_with("::"))
        }
    };
    if !has_body {
        return None;
    }

    // An all-caps "function" with a body is a macro that generates one:
    // SYSCALL_DEFINE3(openat, ...), TEST(Suite, Case). Its first argument
    // is the name a reader looks for.
    if !qualified.contains("::") && is_macro_name(simple) {
        let argument = leading_identifier(first[open + 1..].trim_start());
        return is_identifier(argument).then_some((argument, argument));
    }
    Some((qualified, simple))
}

const ATTRIBUTE_KEYWORDS: &[&str] = &["__attribute__", "__attribute", "__declspec", "alignas"];

/// The parenthesis opening a parameter list: the first `(` that does not
/// open an attribute group such as `__attribute__((unused))`.
fn parameters_open(line: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = line[from..].find('(').map(|at| at + from) {
        let before = line[..at].trim_end();
        let word = &before[after_last(before, |character| {
            !(character.is_ascii_alphanumeric() || character == '_')
        })..];
        if !ATTRIBUTE_KEYWORDS.contains(&word) {
            return Some(at);
        }
        from = line.len() - skip_balanced(&line[at..], '(', ')').len();
    }
    None
}

/// `line` without attribute groups at its end: `static int __attribute__((unused))`
/// ends in a type for the GNU layout's purposes.
fn without_trailing_attributes(line: &str) -> &str {
    let mut line = line.trim_end();
    while let Some(at) = ATTRIBUTE_KEYWORDS
        .iter()
        .filter_map(|keyword| line.rfind(keyword))
        .max()
    {
        let word = leading_identifier(&line[at..]);
        if !skip_balanced(line[at + word.len()..].trim_start(), '(', ')')
            .trim()
            .is_empty()
        {
            break;
        }
        line = line[..at].trim_end();
    }
    line
}

/// `text` after C++ member-function qualifiers: `const`, `noexcept`,
/// `override`, `final`, `volatile`, `&`, `&&`.
fn strip_qualifiers(mut text: &str) -> &str {
    loop {
        let word = leading_identifier(text);
        let skip = if matches!(
            word,
            "const" | "noexcept" | "override" | "final" | "volatile"
        ) {
            word.len()
        } else if text.starts_with('&') {
            1
        } else {
            return text;
        };
        text = text[skip..].trim_start();
    }
}

/// A line of `__name` or `__name(...)` annotations only.
fn is_annotation(mut line: &str) -> bool {
    if line.is_empty() {
        return false;
    }
    while !line.is_empty() {
        let name = leading_identifier(line);
        if !name.starts_with("__") {
            return false;
        }
        line = line[name.len()..].trim_start();
        if line.starts_with('(') {
            line = skip_balanced(line, '(', ')').trim_start();
        }
    }
    true
}

fn next_code_line<'a>(lines: &[&'a str], from: usize) -> &'a str {
    lines[from.min(lines.len())..]
        .iter()
        .map(|line| line.trim())
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

/// The declared name in a typedef's tail: `} name;`, `unsigned long u64;`,
/// `} a_t, *pa_t;`, and for function types the name before the parameter
/// list, found inside a pointer group with any calling convention:
/// `void (*handler_t)(int);`, `int (CDECL *new_handler)(size_t);`.
fn typedef_name(text: &str) -> Option<&str> {
    let text = text.trim().trim_end_matches(';');
    let declared = match text.find('(') {
        Some(open) => {
            let group = &text[open..];
            let close = (group.len() - skip_balanced(group, '(', ')').len())
                .saturating_sub(1)
                .max(1);
            let inner = &group[1..close];
            if inner.contains('*') {
                inner
            } else {
                &text[..open]
            }
        }
        None => text
            .split(',')
            .next()
            .unwrap_or("")
            .split('[')
            .next()
            .unwrap_or(""),
    };
    declared
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .rfind(|part| is_identifier(part) && !part.starts_with("__"))
}

/// Skips `[[...]]`, `__attribute__((...))`, `alignas(...)`, `__declspec(...)`
/// and export macros such as `LLVM_ABI` that sit between a keyword and the
/// name it declares.
fn skip_attributes(mut text: &str) -> &str {
    loop {
        text = text.trim_start();
        if text.starts_with("[[") {
            match text.find("]]") {
                Some(end) => text = &text[end + 2..],
                None => return text,
            }
            continue;
        }
        let word = leading_identifier(text);
        let after = text[word.len()..].trim_start();
        if matches!(word, "__attribute__" | "alignas" | "__declspec") && after.starts_with('(') {
            text = skip_balanced(after, '(', ')');
            continue;
        }
        if is_macro_name(word)
            && after
                .starts_with(|character: char| character.is_ascii_alphabetic() || character == '_')
            && leading_identifier(after) != "final"
        {
            text = after;
            continue;
        }
        return text;
    }
}

/// `text` after the bracketed group it starts with.
pub(crate) fn skip_balanced(text: &str, open: char, close: char) -> &str {
    let mut depth = 0usize;
    for (at, character) in text.char_indices() {
        if character == open {
            depth += 1;
        } else if character == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return &text[at + character.len_utf8()..];
            }
        }
    }
    ""
}

/// The byte offset just past the last character matching `boundary`, or 0;
/// steps over the whole character, which may be several bytes.
fn after_last(text: &str, boundary: impl Fn(char) -> bool) -> usize {
    text.char_indices()
        .rev()
        .find(|&(_, character)| boundary(character))
        .map_or(0, |(at, character)| at + character.len_utf8())
}

fn leading_identifier(text: &str) -> &str {
    let end = text
        .find(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .unwrap_or(text.len());
    &text[..end]
}

fn is_identifier(name: &str) -> bool {
    name.starts_with(|character: char| character.is_ascii_alphabetic() || character == '_')
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn is_macro_name(name: &str) -> bool {
    name.len() >= 2
        && name.chars().any(|character| character.is_ascii_uppercase())
        && name.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

#[cfg(test)]
mod tests {
    use super::parse;

    /// Verification hook for bench/proto/c_defs_check.py: prints each listed
    /// file's definitions as JSON lines.
    /// `C_LIKE_FILES=list cargo test --release c_like::tests::dump -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn dump() {
        let list = std::env::var("C_LIKE_FILES").expect("C_LIKE_FILES names a file of paths");
        for path in std::fs::read_to_string(list).unwrap().lines() {
            let bytes = std::fs::read(path).unwrap_or_default();
            let found = parse(&String::from_utf8_lossy(&bytes));
            let quote = |values: &[String]| {
                values
                    .iter()
                    .map(|value| format!("{value:?}"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            println!(
                "{{\"path\":{path:?},\"definitions\":[{}],\"macros\":[{}]}}",
                quote(&found.definitions),
                quote(&found.macros)
            );
        }
    }

    fn definitions(text: &str) -> Vec<String> {
        let mut found = parse(text);
        found.definitions.extend(found.macros);
        found.definitions
    }

    #[test]
    fn kernel_style_functions_types_and_macros() {
        let source = "\
#ifndef _FOO_H
#define _FOO_H
#include \"internal.h\"
#include <linux/list.h>
#define FOO_MAX 16
#define foo_ready(f) ((f)->ready)

/*
 * not_a_function(void) {
 */
struct foo_dev {
\tint ready;
};

struct foo_dev *foo_lookup(int id);

static int __init foo_probe(struct platform_device *pdev,
\t\t\t    const struct of_device_id *id)
{
\treturn 0;
}

static void __iomem *foo_map(struct foo_dev *dev) {
\treturn NULL;
}

void foo_locked_error(struct foo_dev *dev,
\t\t      int err)
__releases(bitlock)
__acquires(bitlock)
{
}

static bool foo_select(struct foo_dev *dev)
\t__releases(&dev->lock) __acquires(&dev->lock)
{
}
static inline void foo_stub(struct foo_dev *dev) { };
static int __attribute__((unused)) foo_unused(int x) {
}
static int __attribute__((unused))
foo_gnu_unused(int x) {
}

int foo_count; /* see foo_fake(x) {
foo_fake2(void) {
*/
// foo_fake3(void) {
static const char *foo_glob = \"/sys/*\";
int foo_after(void) {
}

SYSCALL_DEFINE3(foo_ctl, int, fd, unsigned int, cmd, unsigned long, arg)
{
\treturn 0;
}

static struct platform_driver foo_driver = {
\t.probe = foo_probe,
};
module_platform_driver(foo_driver);

typedef struct {
\tint x;
} foo_point_t;
typedef unsigned long foo_word;
typedef int (*foo_handler_t)(int);
typedef int (CDECL *foo_new_handler)(size_t size);
typedef enum
{
\tFOO_A,
} foo_version;
enum foo_state { FOO_IDLE, FOO_BUSY };
#endif
";
        assert_eq!(
            definitions(source),
            [
                "struct foo_dev",
                "fn foo_probe",
                "fn foo_map",
                "fn foo_locked_error",
                "fn foo_select",
                "fn foo_stub",
                "fn foo_unused",
                "fn foo_gnu_unused",
                "fn foo_after",
                "fn foo_ctl",
                "typedef foo_point_t",
                "typedef foo_word",
                "typedef foo_handler_t",
                "typedef foo_new_handler",
                "typedef foo_version",
                "enum foo_state",
                "define FOO_MAX",
                "define foo_ready",
            ]
        );
        assert_eq!(parse(source).imports, ["./internal.h"]);
    }

    #[test]
    fn unfinished_lines_do_not_panic() {
        for source in [
            "typedef void (",
            "typedef (",
            "int f(",
            "struct",
            "typedef struct",
            "#define",
            "}",
            "x(\n",
            "static int __attribute__((x",
            "__attribute__((",
            "int\n__attribute__((a)) f(",
        ] {
            parse(source);
        }
    }

    /// Multibyte characters anywhere in a line must not split a slice:
    /// every character boundary of each snippet gets one inserted.
    #[test]
    fn multibyte_text_never_panics() {
        let snippets = [
            "extern \"C\" void test_x(void) {\n}\n",
            "int operator\"\"_us(unsigned long long v) {\n}\n",
            "static int __attribute__((unused)) run(int x) {\n}\n",
            "static int __attribute__((unused))\nrun(int x)\n{\n}\n",
            "typedef int (CDECL *handler_t)(int);\ntypedef struct a {\n} a_t;\n",
            "class LLVM_ABI Name final : public Base {\n};\nName::~Name() {\n}\n",
            "#ifndef G\n#define G\n#define F(x) x\n#include \"a.h\"\n/* c */ // d\n",
            "SYSCALL_DEFINE1(name, int, x)\n__releases(a) __acquires(b)\n{\n}\n",
        ];
        for snippet in snippets {
            for (at, _) in snippet.char_indices().chain([(snippet.len(), ' ')]) {
                for inserted in ["µ", "𐀀", "ƒ("] {
                    parse(&format!("{}{inserted}{}", &snippet[..at], &snippet[at..]));
                }
            }
        }
        // Non-ASCII identifiers are not read (rare; ASCII names around them are).
        assert!(parse("extern \"C\" void test_𐀀(void) {\n}\n")
            .definitions
            .is_empty());
        assert_eq!(
            parse("/* µ */ int ƒ_helper_µ(void);\nint plain(void) {\n}\n").definitions,
            ["fn plain"]
        );
    }

    #[test]
    fn cplusplus_classes_methods_and_gnu_layout() {
        let source = "\
namespace llvm {
class LLVM_ABI SelectionDAG : public Base {
public:
  void inlineMethod() {}
};
template <typename T>
struct DenseMapInfo<T *> {
};
class Forward;
enum class Kind : uint8_t {
  A,
};

SelectionDAG::SelectionDAG(const TargetMachine &TM)
    : TM(TM) {
}

SelectionDAG::~SelectionDAG() {
}

bool SelectionDAG::isKnownNeverZero(SDValue Op) const {
  if (x) {
  }
}

Message Pass::ToMessage()
    const {
}

Context::Context(Status status) :
    status(status) {}

struct Optimizer::Impl {
};
struct LSR final : public UMemory {
};

static int
legacy_style(void)
{
}

TEST(DAGTest, Folds) {
}
} // namespace llvm
";
        assert_eq!(
            definitions(source),
            [
                "class SelectionDAG",
                "struct DenseMapInfo",
                "enum Kind",
                "fn SelectionDAG::SelectionDAG",
                "fn SelectionDAG::~SelectionDAG",
                "fn SelectionDAG::isKnownNeverZero",
                "fn Pass::ToMessage",
                "fn Context::Context",
                "struct Impl",
                "struct LSR",
                "fn legacy_style",
                "fn DAGTest",
            ]
        );
    }
}
