//! Go-style command flags (`-name`, `--name`, `-name=value`, `-name value`),
//! kept so existing scripts and muscle memory keep working.

use crate::{Error, Result};
use std::collections::HashMap;
use std::io::Write;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Bool,
    Text,
    Int,
}

pub struct Flag {
    pub name: &'static str,
    pub kind: Kind,
    pub default: &'static str,
    pub usage: &'static str,
}

pub const fn bool_flag(name: &'static str, usage: &'static str) -> Flag {
    Flag {
        name,
        kind: Kind::Bool,
        default: "false",
        usage,
    }
}

pub const fn text_flag(name: &'static str, default: &'static str, usage: &'static str) -> Flag {
    Flag {
        name,
        kind: Kind::Text,
        default,
        usage,
    }
}

pub const fn int_flag(name: &'static str, usage: &'static str) -> Flag {
    Flag {
        name,
        kind: Kind::Int,
        default: "0",
        usage,
    }
}

pub struct Parsed {
    values: HashMap<&'static str, String>,
    /// Arguments left after the first non-flag argument or `--`.
    pub rest: Vec<String>,
}

impl Parsed {
    pub fn bool(&self, name: &str) -> bool {
        self.values.get(name).is_some_and(|value| value == "true")
    }

    pub fn text(&self, name: &str) -> &str {
        self.values.get(name).map_or("", String::as_str)
    }

    pub fn int(&self, name: &str) -> i64 {
        self.values
            .get(name)
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }
}

/// Parses `args` against `flags`, stopping at the first positional argument.
pub fn parse(command: &str, flags: &[Flag], args: &[String]) -> Result<Parsed> {
    let mut values: HashMap<&'static str, String> = flags
        .iter()
        .map(|flag| (flag.name, flag.default.to_owned()))
        .collect();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg.len() < 2 || !arg.starts_with('-') {
            break;
        }
        index += 1;
        if arg == "--" {
            break;
        }
        let name = arg.strip_prefix("--").unwrap_or(&arg[1..]);
        if name.is_empty() || name.starts_with('-') || name.starts_with('=') {
            return Err(usage_error(
                command,
                flags,
                format!("bad flag syntax: {arg}"),
            ));
        }
        let (name, inline) = match name.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (name, None),
        };
        let Some(flag) = flags.iter().find(|flag| flag.name == name) else {
            if name == "h" || name == "help" {
                print_usage(command, flags);
                return Err(Error::explained("help requested"));
            }
            return Err(usage_error(
                command,
                flags,
                format!("flag provided but not defined: -{name}"),
            ));
        };
        let value = match (flag.kind, inline) {
            (Kind::Bool, None) => "true".to_owned(),
            (Kind::Bool, Some(value)) => match parse_bool(&value) {
                Some(value) => value.to_string(),
                None => {
                    return Err(usage_error(
                        command,
                        flags,
                        format!("invalid boolean value {value:?} for -{name}: parse error"),
                    ));
                }
            },
            (_, Some(value)) => value,
            (_, None) if index < args.len() => {
                index += 1;
                args[index - 1].clone()
            }
            (_, None) => {
                return Err(usage_error(
                    command,
                    flags,
                    format!("flag needs an argument: -{name}"),
                ));
            }
        };
        if flag.kind == Kind::Int && value.parse::<i64>().is_err() {
            return Err(usage_error(
                command,
                flags,
                format!("invalid value {value:?} for flag -{name}: parse error"),
            ));
        }
        values.insert(flag.name, value);
    }
    Ok(Parsed {
        values,
        rest: args[index..].to_vec(),
    })
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

fn usage_error(command: &str, flags: &[Flag], message: String) -> Error {
    let _ = writeln!(std::io::stderr(), "{message}");
    print_usage(command, flags);
    message.into()
}

fn print_usage(command: &str, flags: &[Flag]) {
    let mut sorted: Vec<&Flag> = flags.iter().collect();
    sorted.sort_by_key(|flag| flag.name);
    let mut text = format!("Usage of {command}:\n");
    for flag in sorted {
        text.push_str(&format!("  -{}", flag.name));
        let kind = match flag.kind {
            Kind::Bool => "",
            Kind::Text => " string",
            Kind::Int => " int",
        };
        text.push_str(kind);
        // One-letter booleans fit on the flag's own line.
        if flag.name.len() == 1 && kind.is_empty() {
            text.push('\t');
        } else {
            text.push_str("\n    \t");
        }
        text.push_str(flag.usage);
        match flag.kind {
            Kind::Text if !flag.default.is_empty() => {
                text.push_str(&format!(" (default {:?})", flag.default));
            }
            Kind::Bool if flag.default == "true" => text.push_str(" (default true)"),
            _ => {}
        }
        text.push('\n');
    }
    let _ = std::io::stderr().write_all(text.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAGS: &[Flag] = &[
        bool_flag("yes", "skip confirmation"),
        bool_flag("prune", "prune extra mods"),
        text_flag("memory", "4G", "server memory"),
        int_flag("pid", "process id"),
    ];

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn parses_go_flag_spellings() {
        let parsed = parse(
            "test",
            FLAGS,
            &args(&["--yes", "-memory=6G", "-pid", "42", "-prune=false", "pack"]),
        )
        .unwrap();
        assert!(parsed.bool("yes"));
        assert!(!parsed.bool("prune"));
        assert_eq!(parsed.text("memory"), "6G");
        assert_eq!(parsed.int("pid"), 42);
        assert_eq!(parsed.rest, args(&["pack"]));
    }

    #[test]
    fn stops_at_the_first_positional_argument() {
        let parsed = parse("test", FLAGS, &args(&["pack", "-yes"])).unwrap();
        assert!(!parsed.bool("yes"));
        assert_eq!(parsed.text("memory"), "4G");
        assert_eq!(parsed.rest, args(&["pack", "-yes"]));
    }

    #[test]
    fn rejects_unknown_flags_and_bad_numbers() {
        assert!(parse("test", FLAGS, &args(&["-nope"])).is_err());
        assert!(parse("test", FLAGS, &args(&["-pid", "abc"])).is_err());
        assert!(parse("test", FLAGS, &args(&["-memory"])).is_err());
    }
}
