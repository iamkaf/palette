//! Tests written as functions: each `tests/<name>.mcfunction` runs as `test:<name>` in a
//! vanilla server. A test fails when it returns 0, as `return fail` does, or when it says
//! anything with `say`, so `execute unless ... run say <why>` is a one-line assertion.

use crate::check::files;
use crate::rcon::Rcon;
use crate::server::Log;
use crate::source::{Pack, PackFile};
use crate::{PackRoot, Result};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

/// The namespace test functions live in.
pub const NAMESPACE: &str = "test";

/// Dimensions whose test area Chalk loads before running tests.
const DIMENSIONS: [&str; 3] = [
    "minecraft:overworld",
    "minecraft:the_nether",
    "minecraft:the_end",
];

/// One test function.
#[derive(Clone)]
pub struct FunctionTest {
    /// The function path after `test:`.
    pub name: String,
    pub source: PathBuf,
}

pub enum Outcome {
    Passed,
    /// With what the test said through `say`.
    Failed(Vec<String>),
    /// Minecraft didn't load the function; the problems explain why.
    Missing,
}

pub struct TestResult {
    pub test: FunctionTest,
    pub outcome: Outcome,
}

/// The tests and a pack holding every function under `tests/`: top-level files are tests
/// and files in folders are helpers they can call. `None` when there are no tests.
pub fn find(root: &PackRoot, pack: &Pack) -> Result<Option<(Vec<FunctionTest>, Pack)>> {
    let dir = root.tests_dir();
    if !dir.is_dir() {
        return Ok(None);
    }
    if pack
        .namespaces()
        .iter()
        .any(|namespace| namespace == NAMESPACE)
    {
        return Err(format!(
            "the pack uses the {NAMESPACE} namespace, which Chalk keeps for test functions"
        )
        .into());
    }
    let mut tests = Vec::new();
    let mut functions = Vec::new();
    for source in files(&dir)? {
        let Ok(relative) = source.strip_prefix(&dir) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        let Some(name) = relative.strip_suffix(".mcfunction") else {
            continue;
        };
        if !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-./".contains(&byte)
        }) {
            return Err(format!(
                "{}: function names use lowercase letters, digits, and _ - . /",
                source.display()
            )
            .into());
        }
        if !name.contains('/') {
            tests.push(FunctionTest {
                name: name.to_owned(),
                source: source.clone(),
            });
        }
        functions.push(PackFile {
            path: format!("data/{NAMESPACE}/function/{relative}"),
            source,
        });
    }
    if tests.is_empty() {
        return Ok(None);
    }
    let test_pack = Pack {
        description: "Chalk test functions".into(),
        minecraft: pack.minecraft.clone(),
        formats: pack.formats,
        files: functions,
        overlays: Vec::new(),
    };
    Ok(Some((tests, test_pack)))
}

/// Loads blocks 0 to 31 on X and Z in every vanilla dimension and waits until they are
/// ready. A server without players has no chunks loaded, and a test runs within one tick,
/// too soon to load its own.
pub fn load_test_area(rcon: &mut Rcon) -> Result<()> {
    for dimension in DIMENSIONS {
        rcon.command(&format!(
            "execute in {dimension} run forceload add 0 0 31 31"
        ))?;
    }
    let started = Instant::now();
    loop {
        let mut loaded = true;
        for dimension in DIMENSIONS {
            let reply = rcon.command(&format!(
                "execute in {dimension} if loaded 0 0 0 if loaded 31 0 31"
            ))?;
            loaded &= reply.contains("passed");
        }
        if loaded {
            return Ok(());
        }
        if started.elapsed() > Duration::from_secs(60) {
            return Err("the test area didn't load within a minute".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Runs one test and collects what it said.
pub fn run(rcon: &mut Rcon, log: &mut Log, test: &FunctionTest) -> Result<TestResult> {
    log.read_new()?;
    let reply = rcon.command(&format!("function {NAMESPACE}:{}", test.name))?;
    // `say` lines reach the log just after the reply.
    let said = said(&log.read_until_quiet(Duration::from_millis(200))?);
    let outcome = if reply.starts_with("Unknown function") {
        Outcome::Missing
    } else if returned(&reply) == Some(0) || !said.is_empty() {
        Outcome::Failed(said)
    } else {
        Outcome::Passed
    };
    Ok(TestResult {
        test: test.clone(),
        outcome,
    })
}

/// The value in `Function test:x returned 0`, when the function returned one.
fn returned(reply: &str) -> Option<i64> {
    reply
        .rsplit_once(" returned ")
        .and_then(|(_, value)| value.trim().parse().ok())
}

/// Messages from `say`, which the log shows as `[Rcon] message`.
fn said(log: &str) -> Vec<String> {
    log.lines()
        .filter_map(|line| {
            line.split_once("[Rcon] ")
                .map(|(_, message)| message.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check;
    use std::fs;

    #[test]
    fn top_level_functions_are_tests_and_folders_hold_helpers() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("my-pack");
        fs::create_dir_all(repo.join("datapack/data/demo/function")).expect("pack");
        fs::create_dir_all(repo.join("tests/helpers")).expect("tests");
        fs::write(
            repo.join("chalk.toml"),
            "description = \"Demo\"\nminecraft = \"26.2\"\n",
        )
        .expect("manifest");
        fs::write(
            repo.join("datapack/data/demo/function/hello.mcfunction"),
            "say hi\n",
        )
        .expect("function");
        fs::write(repo.join("tests/hello.mcfunction"), "return 1\n").expect("test");
        fs::write(repo.join("tests/helpers/frame.mcfunction"), "return 1\n").expect("helper");
        fs::write(repo.join("tests/hello.test.ts"), "").expect("TeaKit test");
        let root = PackRoot::at(&repo).expect("root");
        let support = check::support(&root).expect("support");

        let (tests, pack) = find(&root, &support.pack).expect("find").expect("tests");

        assert_eq!(
            tests
                .iter()
                .map(|test| test.name.as_str())
                .collect::<Vec<_>>(),
            ["hello"]
        );
        let mut paths: Vec<&str> = pack.files.iter().map(|file| file.path.as_str()).collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                "data/test/function/hello.mcfunction",
                "data/test/function/helpers/frame.mcfunction"
            ]
        );
    }

    #[test]
    fn replies_and_log_lines_become_results() {
        assert_eq!(
            returned("Running function test:aFunction test:a returned 0"),
            Some(0)
        );
        assert_eq!(returned("Running function test:a"), None);
        assert_eq!(
            said(
                "[14:23:22] [Server thread/INFO]: [Not Secure] [Rcon] no portal at 800 64 0\n[14:23:22] [Server thread/INFO]: other"
            ),
            ["no portal at 800 64 0"]
        );
    }
}
