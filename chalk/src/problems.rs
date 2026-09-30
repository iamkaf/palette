//! Finds what the game said about a pack while loading it, and which source file each
//! problem came from on that version.

use crate::PackRoot;
use crate::pack::PackFormat;
use crate::source::{Pack, PackFile};
use std::path::PathBuf;

/// One problem Minecraft logged about the pack.
#[derive(Debug, PartialEq, Eq)]
pub struct Problem {
    /// The source file the game read, when the message names one of the pack's resources.
    pub source: Option<PathBuf>,
    pub message: String,
}

/// Prints problems under a version heading, with sources relative to the repository.
pub fn print(root: &PackRoot, problems: &[Problem]) {
    for problem in problems {
        match &problem.source {
            Some(source) => {
                let source = source.strip_prefix(root.dir()).unwrap_or(source);
                println!("  PACK    {}: {}", source.display(), problem.message);
            }
            None => println!("  PACK    {}", problem.message),
        }
    }
}

/// What a message is about, as far as the log says.
enum Subject {
    Function(String),
    Advancement(String),
    Tag(String),
    /// An element of a data-driven registry, such as `advancement` in 26.3.
    Element {
        registry: String,
        id: String,
    },
    Other(String),
}

/// Reads a server log for problems with the pack's namespaces, resolved against the files
/// a game with `format` loads.
pub fn find(log: &str, pack: &Pack, format: PackFormat) -> Vec<Problem> {
    let namespaces = pack.namespaces();
    let ours = |id: &str| {
        id.split_once(':')
            .is_some_and(|(namespace, _)| namespaces.iter().any(|ours| ours == namespace))
    };
    let files = pack.files_for(format);
    let lines: Vec<&str> = log.lines().collect();
    let mut problems = Vec::new();
    let mut registry = None;
    for (index, line) in lines.iter().enumerate() {
        let reason = || caused_by(&lines[index + 1..]);
        if let Some(rest) = line.strip_prefix("> Errors in registry ") {
            registry = rest
                .trim_end_matches(':')
                .rsplit(':')
                .next()
                .map(str::to_owned);
            continue;
        }
        if let Some(rest) = line.strip_prefix(">> Errors in element ") {
            let id = rest.trim_end_matches(':').to_owned();
            if ours(&id) {
                let registry = registry.clone().unwrap_or_default();
                problems.push(problem(&files, Subject::Element { registry, id }, reason()));
            }
            continue;
        }
        let Some(message) = logged(line) else {
            continue;
        };
        if message.starts_with("Failed to load datapacks, can't proceed with server load") {
            problems.push(Problem {
                source: None,
                message: "Minecraft refused to start with the pack loaded".into(),
            });
        } else if let Some(rest) = message.strip_prefix("Couldn't load tag ") {
            let (id, detail) = rest.split_once(" as it is ").unwrap_or((rest, ""));
            if ours(id) {
                let detail = detail.split(" (from ").next().unwrap_or(detail);
                problems.push(problem(
                    &files,
                    Subject::Tag(id.to_owned()),
                    detail.to_owned(),
                ));
            }
        } else if let Some(id) = message.strip_prefix("Failed to load function ") {
            if ours(id) {
                problems.push(problem(&files, Subject::Function(id.to_owned()), reason()));
            }
        } else if let Some(rest) = message.strip_prefix("Parsing error loading custom advancement ")
        {
            let (id, detail) = rest.split_once(": ").unwrap_or((rest, ""));
            if ours(id) {
                problems.push(problem(
                    &files,
                    Subject::Advancement(id.to_owned()),
                    trim_input(detail),
                ));
            }
        } else if message.starts_with("Not all defined tags") {
            // Follows a tag that failed to load, which is reported on its own.
        } else if let Some(id) = mentioned_id(message, &ours) {
            problems.push(problem(&files, Subject::Other(id), message.to_owned()));
        }
    }
    problems
}

/// The message of a `[time] [thread/ERROR]:` or `WARN` log line.
fn logged(line: &str) -> Option<&str> {
    ["/ERROR]: ", "/WARN]: "]
        .iter()
        .find_map(|level| line.split_once(level).map(|(_, message)| message))
}

/// The message of the first `Caused by:` line, without its exception class.
fn caused_by(following: &[&str]) -> String {
    following
        .iter()
        .take(60)
        .find_map(|line| line.trim().strip_prefix("Caused by: "))
        .map(|cause| trim_input(cause.split_once(": ").map_or(cause, |(_, message)| message)))
        .unwrap_or_else(|| "see the server log".into())
}

/// Drops the JSON dump Minecraft appends after `missed input:`.
fn trim_input(message: &str) -> String {
    message
        .split(" missed input:")
        .next()
        .unwrap_or(message)
        .trim()
        .to_owned()
}

fn mentioned_id(message: &str, ours: &impl Fn(&str) -> bool) -> Option<String> {
    message
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':')))
        .find(|word| ours(word))
        .map(str::to_owned)
}

fn problem(files: &[&PackFile], subject: Subject, message: String) -> Problem {
    let source = source(files, &subject).map(|file| file.source.clone());
    let message = match (&subject, &source) {
        (_, Some(_)) => message,
        (
            Subject::Function(id)
            | Subject::Advancement(id)
            | Subject::Tag(id)
            | Subject::Other(id),
            None,
        )
        | (Subject::Element { id, .. }, None) => format!("{id}: {message}"),
    };
    Problem { source, message }
}

fn source<'a>(files: &[&'a PackFile], subject: &Subject) -> Option<&'a PackFile> {
    let (id, folder): (&str, Option<String>) = match subject {
        Subject::Function(id) => (id, Some("function".into())),
        Subject::Advancement(id) => (id, Some("advancement".into())),
        Subject::Element { registry, id } => (id, Some(registry.clone())),
        Subject::Tag(id) | Subject::Other(id) => (id, None),
    };
    let (namespace, path) = id.split_once(':')?;
    let tag = matches!(subject, Subject::Tag(_));
    let mut matches = files.iter().filter(|file| {
        let Some(rest) = file.path.strip_prefix(&format!("data/{namespace}/")) else {
            return false;
        };
        let Some((without_extension, _)) = rest.rsplit_once('.') else {
            return false;
        };
        match &folder {
            Some(folder) => without_extension == format!("{folder}/{path}"),
            None if tag => without_extension
                .strip_prefix("tags/")
                .and_then(|rest| rest.split_once('/'))
                .is_some_and(|(_, tag_path)| tag_path == path),
            None => without_extension
                .split_once('/')
                .is_some_and(|(_, rest)| rest == path),
        }
    });
    let found = matches.next()?;
    // Two files could fit a bare ID; don't guess between them.
    matches.next().is_none().then_some(*found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::FormatRange;
    use crate::source::Overlay;
    use crate::versions::VersionRange;

    fn file(path: &str, source: &str) -> PackFile {
        PackFile {
            source: PathBuf::from(source),
            path: path.into(),
        }
    }

    fn pack() -> Pack {
        let ns = "data/portals";
        Pack {
            description: String::new(),
            minecraft: VersionRange::parse("1.21.1-26.3").expect("range"),
            formats: FormatRange {
                min: PackFormat {
                    major: 48,
                    minor: 0,
                },
                max: PackFormat {
                    major: 121,
                    minor: u32::MAX,
                },
            },
            files: vec![
                file(&format!("{ns}/advancement/light.json"), "light.json"),
                file(&format!("{ns}/function/ray.mcfunction"), "ray.mcfunction"),
                file(&format!("{ns}/tags/block/space.json"), "space.json"),
            ],
            overlays: vec![Overlay {
                directory: "1.21-26.2".into(),
                formats: FormatRange {
                    min: PackFormat {
                        major: 48,
                        minor: 0,
                    },
                    max: PackFormat {
                        major: 107,
                        minor: u32::MAX,
                    },
                },
                files: vec![file(
                    &format!("{ns}/advancement/light.json"),
                    "light@-26.2.json",
                )],
            }],
        }
    }

    const OLD: PackFormat = PackFormat {
        major: 48,
        minor: 0,
    };
    const NEW: PackFormat = PackFormat {
        major: 121,
        minor: 0,
    };

    #[test]
    fn old_versions_report_functions_tags_and_advancements_against_their_variants() {
        let log = "\
[13:09:00] [Worker-Main-1/ERROR]: Couldn't load tag portals:space as it is missing following references: minecraft:not_a_block (from file/pack.zip)
[13:09:00] [main/ERROR]: Failed to load function portals:ray
java.util.concurrent.CompletionException: java.lang.IllegalArgumentException: Whilst parsing command on line 1: Unknown or incomplete command, see below for error at position 0: <--[HERE]
\tat java.base/java.util.concurrent.CompletableFuture.encodeThrowable(CompletableFuture.java:315) ~[?:?]
Caused by: java.lang.IllegalArgumentException: Whilst parsing command on line 1: Unknown or incomplete command, see below for error at position 0: <--[HERE]
\tat ig.a(SourceFile:80) ~[server-1.21.1.jar:?]
[13:09:00] [main/ERROR]: Parsing error loading custom advancement portals:light: Unknown registry key in ResourceKey[minecraft:root / minecraft:trigger_type]: minecraft:no_such_trigger missed input: {\"used\":{}}
[13:09:00] [main/WARN]: Not all defined tags for registry ResourceKey[minecraft:root / minecraft:block] are present in data pack: portals:space
[13:09:00] [Server thread/WARN]: **** SERVER IS RUNNING IN OFFLINE/INSECURE MODE!";

        let problems = find(log, &pack(), OLD);

        assert_eq!(
            problems,
            vec![
                Problem { source: Some("space.json".into()), message: "missing following references: minecraft:not_a_block".into() },
                Problem {
                    source: Some("ray.mcfunction".into()),
                    message: "Whilst parsing command on line 1: Unknown or incomplete command, see below for error at position 0: <--[HERE]".into()
                },
                Problem {
                    source: Some("light@-26.2.json".into()),
                    message: "Unknown registry key in ResourceKey[minecraft:root / minecraft:trigger_type]: minecraft:no_such_trigger".into()
                },
            ]
        );
    }

    #[test]
    fn a_registry_error_that_stops_the_server_names_its_file() {
        let log = "\
[13:08:45] [Worker-Main-8/ERROR]: Registry loading errors:
> Errors in registry minecraft:advancement:
>> Errors in element portals:light:
java.lang.IllegalStateException: Failed to parse portals:light from pack file/pack.zip
\tat net.minecraft.resources.RegistryLoadTask$PendingRegistration.loadFromResource(RegistryLoadTask.java:111)
Caused by: java.lang.IllegalStateException: Unknown registry key in ResourceKey[minecraft:root / minecraft:trigger_type]: minecraft:no_such_trigger missed input: {}
[13:08:45] [main/WARN]: Failed to load datapacks, can't proceed with server load. You can either fix your datapacks or reset to vanilla with --safeMode";

        let problems = find(log, &pack(), NEW);

        assert_eq!(problems.len(), 2);
        assert_eq!(
            problems[0].source.as_deref(),
            Some(std::path::Path::new("light.json"))
        );
        assert_eq!(
            problems[0].message,
            "Unknown registry key in ResourceKey[minecraft:root / minecraft:trigger_type]: minecraft:no_such_trigger"
        );
        assert_eq!(
            problems[1].message,
            "Minecraft refused to start with the pack loaded"
        );
    }

    #[test]
    fn other_packs_and_vanilla_are_not_our_problem() {
        let log = "[13:09:00] [main/ERROR]: Failed to load function other:thing\n\
[13:09:00] [main/WARN]: Couldn't load tag minecraft:foo as it is missing following references: minecraft:bar";
        assert!(find(log, &pack(), NEW).is_empty());
    }
}
