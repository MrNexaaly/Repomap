use repomap::{
    overview, repo_map, repo_map_with_detail, repomap_ranker::RankContext, Detail, ToolResult,
};
use std::{
    env,
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

const HELP: &str = r#"Native query-aware repository map

Usage:
  repomap [DIRECTORY] [--token-budget N]          Overview for an agent new to the repository
  repomap [DIRECTORY] --query TEXT [CONTEXT OPTIONS]   Files ranked for one task
  repomap [DIRECTORY] --list [--max-chars N]       Legacy list: entry points, then size
  repomap mcp                                     Serve the maps as MCP tools over stdio

The overview (the default) states the purpose, stack, build and test commands,
layout with each part's purpose, entry points, central files and a compact
index of every file, in about 2000 tokens.

Options:
      --overview               The default map, named explicitly
      --list                   The legacy entrypoint-then-size list
      --compact                Task map with one line per file (more candidates)
  -q, --query TEXT             Rank files for this task or question
      --mentioned-path PATH    Prioritize an explicitly mentioned path (repeatable)
      --mentioned-symbol NAME  Prioritize an explicitly mentioned symbol (repeatable)
      --open-path PATH         Boost a currently open path (repeatable)
      --token-budget N         Query-aware map budget in estimated tokens
      --max-chars N            Maximum emitted characters (default: 12000)
  -h, --help                   Show this help
      --version                Show the native repomap version

Any context option (query, mentioned or open paths, token budget with a
query) selects the task map.
"#;

#[derive(Debug, PartialEq, Eq)]
struct Cli {
    directory: PathBuf,
    max_chars: usize,
    query: String,
    mentioned_paths: Vec<String>,
    mentioned_symbols: Vec<String>,
    open_paths: Vec<String>,
    token_budget: Option<usize>,
    uses_context: bool,
    overview: bool,
    list: bool,
    compact: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Run(Cli),
    Help,
    Version,
}

fn option_text(args: &mut impl Iterator<Item = OsString>, option: &str) -> Result<String, String> {
    let value = args
        .next()
        .ok_or_else(|| format!("{option} requires a value"))?;
    value
        .into_string()
        .map_err(|_| format!("{option} requires UTF-8 text"))
}

fn integer(option: &str, value: &str, allow_zero: bool) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("{option} must be an integer"))?;
    if !allow_zero && parsed == 0 {
        return Err(format!("{option} must be at least 1"));
    }
    Ok(parsed)
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Parsed, String> {
    let mut args = args.into_iter();
    let mut directory = None;
    let mut max_chars = 12_000;
    let mut query = String::new();
    let mut mentioned_paths = Vec::new();
    let mut mentioned_symbols = Vec::new();
    let mut open_paths = Vec::new();
    let mut token_budget = None;
    let mut uses_context = false;
    let mut overview = false;
    let mut list = false;
    let mut compact = false;
    let mut positional_only = false;

    while let Some(raw) = args.next() {
        let text = raw.to_str();
        if !positional_only && text == Some("--") {
            positional_only = true;
            continue;
        }
        if !positional_only {
            match text {
                Some("-h" | "--help") => return Ok(Parsed::Help),
                Some("--version") => return Ok(Parsed::Version),
                Some("--overview") => {
                    overview = true;
                    continue;
                }
                Some("--list") => {
                    list = true;
                    continue;
                }
                Some("--compact") => {
                    compact = true;
                    continue;
                }
                Some("-q" | "--query") => {
                    query = option_text(&mut args, "--query")?;
                    uses_context = true;
                    continue;
                }
                Some("--mentioned-path") => {
                    mentioned_paths.push(option_text(&mut args, "--mentioned-path")?);
                    uses_context = true;
                    continue;
                }
                Some("--mentioned-symbol") => {
                    mentioned_symbols.push(option_text(&mut args, "--mentioned-symbol")?);
                    uses_context = true;
                    continue;
                }
                Some("--open-path") => {
                    open_paths.push(option_text(&mut args, "--open-path")?);
                    uses_context = true;
                    continue;
                }
                Some("--token-budget") => {
                    let value = option_text(&mut args, "--token-budget")?;
                    token_budget = Some(integer("--token-budget", &value, true)?);
                    uses_context = true;
                    continue;
                }
                Some("--max-chars") => {
                    let value = option_text(&mut args, "--max-chars")?;
                    max_chars = integer("--max-chars", &value, false)?;
                    continue;
                }
                Some(value) if value.starts_with("--query=") => {
                    query = value["--query=".len()..].to_owned();
                    uses_context = true;
                    continue;
                }
                Some(value) if value.starts_with("--mentioned-path=") => {
                    mentioned_paths.push(value["--mentioned-path=".len()..].to_owned());
                    uses_context = true;
                    continue;
                }
                Some(value) if value.starts_with("--mentioned-symbol=") => {
                    mentioned_symbols.push(value["--mentioned-symbol=".len()..].to_owned());
                    uses_context = true;
                    continue;
                }
                Some(value) if value.starts_with("--open-path=") => {
                    open_paths.push(value["--open-path=".len()..].to_owned());
                    uses_context = true;
                    continue;
                }
                Some(value) if value.starts_with("--token-budget=") => {
                    token_budget = Some(integer(
                        "--token-budget",
                        &value["--token-budget=".len()..],
                        true,
                    )?);
                    uses_context = true;
                    continue;
                }
                Some(value) if value.starts_with("--max-chars=") => {
                    max_chars = integer("--max-chars", &value["--max-chars=".len()..], false)?;
                    continue;
                }
                Some(value) if value.starts_with('-') => {
                    return Err(format!("unknown option: {value}"));
                }
                _ => {}
            }
        }

        if directory.replace(PathBuf::from(raw)).is_some() {
            return Err("only one directory may be supplied".into());
        }
    }

    Ok(Parsed::Run(Cli {
        directory: directory.unwrap_or_else(|| PathBuf::from(".")),
        max_chars,
        query,
        mentioned_paths,
        mentioned_symbols,
        open_paths,
        token_budget,
        uses_context,
        overview,
        list,
        compact,
    }))
}

fn write_result(result: ToolResult) -> ExitCode {
    let mut stream: Box<dyn Write> = if result.ok {
        Box::new(io::stdout().lock())
    } else {
        Box::new(io::stderr().lock())
    };
    let write = stream.write_all(result.output.as_bytes()).and_then(|_| {
        if result.output.ends_with('\n') {
            Ok(())
        } else {
            stream.write_all(b"\n")
        }
    });
    if let Err(error) = write {
        if error.kind() == io::ErrorKind::BrokenPipe {
            return ExitCode::SUCCESS;
        }
        let _ = writeln!(io::stderr(), "repomap: failed to write output: {error}");
        return ExitCode::from(1);
    }
    ExitCode::from(if result.ok { 0 } else { 1 })
}

fn main() -> ExitCode {
    if env::args_os().nth(1).is_some_and(|first| first == "mcp") {
        return match repomap::mcp::serve() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("repomap mcp: {error}");
                ExitCode::from(1)
            }
        };
    }
    let parsed = match parse(env::args_os().skip(1)) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("repomap: {error}\nTry 'repomap --help' for usage.");
            return ExitCode::from(2);
        }
    };
    let cli = match parsed {
        Parsed::Help => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Parsed::Version => {
            println!("repomap {} (native Rust)", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Parsed::Run(cli) => cli,
    };
    if !cli.directory.is_dir() {
        eprintln!("repomap: not a directory: {}", cli.directory.display());
        return ExitCode::from(2);
    }
    let task = cli.uses_context
        && (!cli.query.is_empty()
            || !cli.mentioned_paths.is_empty()
            || !cli.mentioned_symbols.is_empty()
            || !cli.open_paths.is_empty());
    if [cli.overview, cli.list, task]
        .iter()
        .filter(|chosen| **chosen)
        .count()
        > 1
    {
        eprintln!("repomap: --overview, --list and a task query are separate maps; choose one");
        return ExitCode::from(2);
    }
    let result = if cli.list {
        repo_map(&cli.directory, cli.max_chars)
    } else if !task {
        overview(
            &cli.directory,
            cli.token_budget.unwrap_or(2000),
            cli.max_chars,
        )
    } else if cli.uses_context {
        repo_map_with_detail(
            &cli.directory,
            cli.max_chars,
            &RankContext {
                query: cli.query,
                mentioned_paths: cli.mentioned_paths,
                mentioned_symbols: cli.mentioned_symbols,
                open_paths: cli.open_paths,
                token_budget: cli
                    .token_budget
                    .unwrap_or_else(|| cli.max_chars.div_ceil(4)),
            },
            if cli.compact {
                Detail::Compact
            } else {
                Detail::Full
            },
        )
    } else {
        repo_map(&cli.directory, cli.max_chars)
    };
    write_result(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn legacy_invocation_keeps_its_interface_and_defaults() {
        let Parsed::Run(cli) = parse(strings(&["src", "--max-chars", "9000"])).unwrap() else {
            panic!("expected runnable CLI")
        };
        assert_eq!(cli.directory, PathBuf::from("src"));
        assert_eq!(cli.max_chars, 9_000);
        assert!(!cli.uses_context);
        assert_eq!(cli.token_budget, None);
    }

    #[test]
    fn query_context_flags_are_repeatable_and_activate_native_ranking() {
        let Parsed::Run(cli) = parse(strings(&[
            ".",
            "--query=authorize payment",
            "--mentioned-symbol",
            "authorize_payment",
            "--mentioned-path=payments.rs",
            "--mentioned-path",
            "checkout.rs",
            "--open-path",
            "lib.rs",
            "--token-budget=35",
        ]))
        .unwrap() else {
            panic!("expected runnable CLI")
        };
        assert!(cli.uses_context);
        assert_eq!(cli.query, "authorize payment");
        assert_eq!(cli.mentioned_symbols, ["authorize_payment"]);
        assert_eq!(cli.mentioned_paths, ["payments.rs", "checkout.rs"]);
        assert_eq!(cli.open_paths, ["lib.rs"]);
        assert_eq!(cli.token_budget, Some(35));
    }

    #[test]
    fn invalid_budgets_and_unknown_options_fail_closed() {
        assert_eq!(
            parse(strings(&["--max-chars", "0"])).unwrap_err(),
            "--max-chars must be at least 1"
        );
        assert_eq!(
            parse(strings(&["--token-budget", "many"])).unwrap_err(),
            "--token-budget must be an integer"
        );
        assert_eq!(
            parse(strings(&["--semantic-magic"])).unwrap_err(),
            "unknown option: --semantic-magic"
        );
    }
}
