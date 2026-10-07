//! TypeScript and JavaScript definitions, also inside the `<script>` blocks
//! of Svelte and Vue components: top-level functions, classes and their
//! methods, interfaces, type aliases, enums and variables, plus relative
//! imports. "Top level" is the script's own base indentation, because
//! formatters indent a component's script one level.

use crate::c_like::Definitions;

const DECLARATIONS: &[(&str, &str)] = &[
    ("async function* ", "function"),
    ("async function ", "function"),
    ("function* ", "function"),
    ("function ", "function"),
    ("abstract class ", "class"),
    ("class ", "class"),
    ("interface ", "interface"),
    ("type ", "type"),
    ("const enum ", "enum"),
    ("enum ", "enum"),
    ("const ", "const"),
    ("let ", "let"),
    ("var ", "var"),
];

const MEMBER_MODIFIERS: &[&str] = &[
    "public ", "private ", "protected ", "static ", "async ", "override ", "readonly ", "abstract ",
    "declare ", "get ", "set ", "*",
];

struct Class {
    name: String,
    indent: usize,
    member_indent: Option<usize>,
}

/// `component` is true for `.svelte`/`.vue`: only `<script>` blocks count.
pub(crate) fn parse(text: &str, component: bool) -> Definitions {
    let mut found = Definitions::default();
    let mut in_script = !component;
    let mut base = (!component).then_some(0);
    let mut class: Option<Class> = None;
    let mut in_comment = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if component {
            if !in_script {
                if trimmed.starts_with("<script") && !trimmed.contains("</script") && !trimmed.ends_with("/>") {
                    (in_script, base, class) = (true, None, None);
                }
                continue;
            }
            if trimmed.starts_with("</script") {
                in_script = false;
                continue;
            }
        }
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
        if let Some(target) = crate::repo_map::import_literal(trimmed) {
            if target.starts_with('.') || target.starts_with('/') {
                found.imports.push(target);
            }
        }
        let width = line.len() - line.trim_start().len();
        let base_width = *base.get_or_insert(width);
        if let Some(open) = &mut class {
            if width <= open.indent && trimmed.starts_with('}') {
                class = None;
                continue;
            }
            if width > open.indent {
                if width == *open.member_indent.get_or_insert(width) {
                    if let Some(method) = member_name(trimmed) {
                        found.definitions.push(format!("method {}.{method}", open.name));
                        found.symbols.push(method.to_owned());
                    }
                }
                continue;
            }
            class = None;
        }
        if width != base_width {
            continue;
        }
        let mut stripped = trimmed;
        for prefix in ["export default ", "export ", "declare "] {
            stripped = stripped.strip_prefix(prefix).unwrap_or(stripped);
        }
        for (prefix, kind) in DECLARATIONS {
            let Some(rest) = stripped.strip_prefix(prefix) else {
                continue;
            };
            let name = identifier(rest.trim_start());
            if !name.is_empty() && !(*kind == "class" && name == "extends") {
                found.definitions.push(format!("{kind} {name}"));
                found.symbols.push(name.to_owned());
                if *kind == "class" && !trimmed.ends_with('}') {
                    class = Some(Class { name: name.to_owned(), indent: width, member_indent: None });
                }
            }
            break;
        }
    }
    found
}

/// The method a class-body line declares: `async load(id) {`,
/// `static get count(): number {`, `#reset() {`, `render<T>(x: T) {`.
/// Fields (`handler = () => {}`), the constructor and decorators are not methods.
fn member_name(line: &str) -> Option<&str> {
    let mut rest = line;
    while let Some(next) = MEMBER_MODIFIERS.iter().find_map(|modifier| rest.strip_prefix(modifier)) {
        rest = next.trim_start();
    }
    let rest = rest.strip_prefix('#').unwrap_or(rest);
    let name = identifier(rest);
    if name.is_empty() || name == "constructor" {
        return None;
    }
    let mut after = rest[name.len()..].trim_start();
    after = after.strip_prefix('?').unwrap_or(after).trim_start();
    if after.starts_with('<') {
        after = crate::c_like::skip_balanced(after, '<', '>').trim_start();
    }
    after.starts_with('(').then_some(name)
}

fn identifier(text: &str) -> &str {
    let end = text
        .find(|character: char| !(character.is_alphanumeric() || character == '_' || character == '$'))
        .unwrap_or(text.len());
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn module_declarations_and_class_methods() {
        let source = "\
import { helper } from './helper';
/**
 * not_a_function(x) {
 */
export default async function load(id: string) {
  const inner = 1;
}
export abstract class Store<T> extends Base {
  private items: T[] = [];
  handler = () => {};
  @Input() name: string;
  constructor() {}
  async fetch(id: string): Promise<T> {
    if (id) {
      call(id);
    }
  }
  static get count(): number { return 0; }
  #reset() {}
  render<U>(x: U) {}
  abstract save(): void;
}
export interface Shape { area(): number }
export type Id = string;
export const enum Mode { A }
declare const VERSION: string;
let counter = 0;
var legacy = require('./legacy');
const { a, b } = pair;
";
        let found = parse(source, false);
        assert_eq!(
            found.definitions,
            [
                "function load",
                "class Store",
                "method Store.fetch",
                "method Store.count",
                "method Store.reset",
                "method Store.render",
                "method Store.save",
                "interface Shape",
                "type Id",
                "enum Mode",
                "const VERSION",
                "let counter",
                "var legacy",
            ]
        );
        assert_eq!(found.imports, ["./helper", "./legacy"]);
    }

    #[test]
    fn component_scripts_are_read_at_their_own_indentation() {
        let source = "\
<script lang=\"ts\">
\timport Row from './Row.svelte';
\tlet { items } = $props();
\tconst total = $derived(items.length);
\tfunction select(id: string) {
\t\tconst local = id;
\t}
</script>

<div>
function notCode() {}
</div>
<script context=\"module\">
  export const prerender = true;
</script>
";
        let found = parse(source, true);
        assert_eq!(found.definitions, ["const total", "function select", "const prerender"]);
        assert_eq!(found.imports, ["./Row.svelte"]);
    }
}
