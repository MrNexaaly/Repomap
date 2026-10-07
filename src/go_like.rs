//! Go definitions: functions, methods (shown with their receiver type), and
//! the names declared by `type`, `var` and `const`, grouped `( ... )`
//! declarations included.

use crate::c_like::Definitions;

pub(crate) fn parse(text: &str) -> Definitions {
    let mut found = Definitions::default();
    // Inside `const (`: the kind and the indentation of its items.
    let mut group: Option<(&'static str, Option<usize>)> = None;
    let mut in_comment = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if in_comment {
            in_comment = !trimmed.contains("*/");
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("/*") {
            in_comment = !rest.contains("*/");
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let width = line.len() - line.trim_start().len();
        if let Some((kind, indent)) = &mut group {
            if width == 0 && trimmed.starts_with(')') {
                group = None;
            } else if width == *indent.get_or_insert(width) {
                declared(kind, trimmed, &mut found);
            }
            continue;
        }
        if width != 0 {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("func ") {
            function(rest.trim_start(), &mut found);
            continue;
        }
        for kind in ["type", "var", "const"] {
            let Some(rest) = trimmed
                .strip_prefix(kind)
                .filter(|rest| rest.starts_with([' ', '(']))
            else {
                continue;
            };
            let rest = rest.trim_start();
            match rest.strip_prefix('(') {
                Some(inside) if inside.trim().is_empty() => group = Some((kind, None)),
                Some(_) => {}
                None => declared(kind, rest, &mut found),
            }
            break;
        }
    }
    found
}

/// `Name(...)` or `(r *Receiver[T]) Name(...)`.
fn function(rest: &str, found: &mut Definitions) {
    if !rest.starts_with('(') {
        add(found, "func", identifier(rest), None);
        return;
    }
    let after = crate::c_like::skip_balanced(rest, '(', ')');
    let receiver = &rest[1..rest.len() - after.len()];
    let owner = receiver
        .trim_end_matches(')')
        .split_whitespace()
        .last()
        .map_or("", |token| identifier(token.trim_start_matches('*')));
    add(found, "func", identifier(after.trim_start()), Some(owner));
}

/// The names one declaration line introduces: `Name struct {`,
/// `A, B int = 1, 2`, `Name = iota`.
fn declared(kind: &str, text: &str, found: &mut Definitions) {
    let text = text.split("//").next().unwrap_or("");
    if kind == "type" {
        add(found, kind, identifier(text), None);
        return;
    }
    for part in text.split('=').next().unwrap_or("").split(',') {
        add(found, kind, identifier(part.trim_start()), None);
    }
}

fn add(found: &mut Definitions, kind: &str, name: &str, owner: Option<&str>) {
    if name.is_empty()
        || name == "_"
        || name.starts_with(|character: char| character.is_ascii_digit())
    {
        return;
    }
    found.definitions.push(match owner {
        Some(owner) if !owner.is_empty() => format!("{kind} {owner}.{name}"),
        _ => format!("{kind} {name}"),
    });
    found.symbols.push(name.to_owned());
}

fn identifier(text: &str) -> &str {
    let end = text
        .find(|character: char| !(character.is_alphanumeric() || character == '_'))
        .unwrap_or(text.len());
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn functions_methods_and_grouped_declarations() {
        let source = "\
package demo

// func Commented() {}
/*
func Hidden() {}
*/
func Plain(x int) error {
\treturn nil
}

func (r *Reader) Read(p []byte) (int, error) {
\treturn 0, nil
}

func (s Set[T]) Has(v T) bool { return true }

func Map[T any](xs []T) []T { return xs }

type Reader struct {
\tbuf []byte
}

type (
\tAlias = Reader
\tHolder struct {
\t\tinner int
\t}
)

const Single = 3

const (
\tFirst Kind = iota // first, second
\tSecond
\t_
)

var (
\tx, y = 1, 2
\tz int
)

var Global = map[string]int{\"a\": 1, \"b\": 2}
";
        assert_eq!(
            parse(source).definitions,
            [
                "func Plain",
                "func Reader.Read",
                "func Set.Has",
                "func Map",
                "type Reader",
                "type Alias",
                "type Holder",
                "const Single",
                "const First",
                "const Second",
                "var x",
                "var y",
                "var z",
                "var Global",
            ]
        );
    }
}
