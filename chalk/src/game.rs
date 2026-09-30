//! Loads the pack in a vanilla dedicated server for each Minecraft version, runs its test
//! functions there, and reports what the game said about it.

use crate::check::Support;
use crate::function_tests::{self, FunctionTest, Outcome, TestResult};
use crate::modstage::{self, Instance, Loader};
use crate::problems::{self, Problem};
use crate::server::{Console, Server, Started};
use crate::source::Pack;
use crate::versions::Minecraft;
use crate::{PackRoot, Result, build};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;

/// What happened when one Minecraft version loaded the pack.
pub struct Loaded {
    pub minecraft: Minecraft,
    pub problems: Vec<Problem>,
    pub tests: Vec<TestResult>,
    pub log: PathBuf,
}

/// Boots a vanilla server with the built zip, and the test functions when there are any,
/// for each target, a few at a time. Returns results in the order of `targets`.
pub fn load(root: &PackRoot, support: &Support, targets: &[Minecraft]) -> Result<Vec<Loaded>> {
    let archive = build::build(root, &support.pack)?;
    let tests = function_tests::find(root, &support.pack)?;
    let dir = root.build_dir();
    let test_pack_dir = dir.join("test-pack");
    if let Some((_, pack)) = &tests {
        build::unpack(pack, &test_pack_dir)?;
    }
    fs::create_dir_all(dir.join("logs"))?;
    let config = dir.join("modstage-load.toml");
    let mut consoles = Vec::new();
    let mut instances = Vec::new();
    for minecraft in targets {
        let console = Console::new()?;
        let mut packs = vec![(archive.as_path(), format!("{}.zip", root.slug()))];
        if tests.is_some() {
            packs.push((test_pack_dir.as_path(), format!("{}-tests", root.slug())));
        }
        instances.push(Instance {
            name: instance(minecraft),
            minecraft: &minecraft.version,
            loader: Loader::Vanilla,
            sides: &["server"],
            mods: Vec::new(),
            // Several servers run at once, so each needs its own ports.
            properties: std::iter::once(("server-port", modstage::free_port()?.to_string()))
                .chain(console.properties())
                .collect(),
            packs,
        });
        consoles.push(console);
    }
    fs::write(&config, modstage::config(root.slug(), &instances))?;

    let pending = Mutex::new(targets.iter().zip(&consoles).collect::<Vec<_>>());
    let results = Mutex::new(Vec::new());
    let workers = thread::available_parallelism()
        .map_or(1, |cores| cores.get() / 4)
        .clamp(1, 3)
        .min(targets.len());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while let Some((minecraft, console)) =
                    pending.lock().ok().and_then(|mut pending| pending.pop())
                {
                    let loaded =
                        load_one(root, support, tests.as_ref(), &config, minecraft, console);
                    if let Ok(mut results) = results.lock() {
                        results.push((minecraft.version.clone(), loaded));
                    }
                }
            });
        }
    });

    let mut results = results
        .into_inner()
        .map_err(|_| "a load check thread panicked")?;
    let mut ordered = Vec::new();
    for minecraft in targets {
        let index = results
            .iter()
            .position(|(version, _)| version == &minecraft.version)
            .ok_or_else(|| format!("no load result for {}", minecraft.version))?;
        ordered.push(results.remove(index).1?);
    }
    Ok(ordered)
}

/// Prints one version's result. Returns whether it loaded cleanly and passed its tests.
pub fn print(root: &PackRoot, loaded: &Loaded) -> bool {
    let failed: Vec<&TestResult> = loaded
        .tests
        .iter()
        .filter(|result| !matches!(result.outcome, Outcome::Passed))
        .collect();
    let mut summary = vec![match loaded.problems.len() {
        0 => "loaded".to_owned(),
        1 => "1 problem".to_owned(),
        count => format!("{count} problems"),
    }];
    match (loaded.tests.len(), failed.len()) {
        (0, _) => {}
        (1, 0) => summary.push("1 test passed".into()),
        (total, 0) => summary.push(format!("{total} tests passed")),
        (total, failed) => summary.push(format!("{failed} of {total} tests failed")),
    }
    println!("  {:<8} {}", loaded.minecraft.version, summary.join(", "));
    problems::print(root, &loaded.problems);
    for result in &failed {
        let source = result
            .test
            .source
            .strip_prefix(root.dir())
            .unwrap_or(&result.test.source);
        match &result.outcome {
            Outcome::Failed(said) => {
                println!("  FAIL    {}", source.display());
                for line in said {
                    println!("          {line}");
                }
            }
            Outcome::Missing => {
                println!("  FAIL    {}: Minecraft didn't load it", source.display())
            }
            Outcome::Passed => {}
        }
    }
    loaded.problems.is_empty() && failed.is_empty()
}

fn instance(minecraft: &Minecraft) -> String {
    format!("load-{}", minecraft.version)
}

fn load_one(
    root: &PackRoot,
    support: &Support,
    tests: Option<&(Vec<FunctionTest>, Pack)>,
    config: &Path,
    minecraft: &Minecraft,
    console: &Console,
) -> Result<Loaded> {
    let name = instance(minecraft);
    let log = root.build_dir().join("logs").join(format!("{name}.log"));
    modstage::stop_leftovers(config, &name)?;
    // Every check starts from a new world, so tests never see what an earlier run left.
    if let Some(dir) = modstage::instance_dir(config, &name)? {
        let world = dir.join("server").join("game").join("world");
        if world.exists() {
            fs::remove_dir_all(world)?;
        }
    }

    let mut server = Server::start(config, &name, log.clone())?;
    let (text, ready, results) = match server.wait_ready()? {
        Started::Ready(mut text) => {
            let mut rcon = console.connect()?;
            let mut results = Vec::new();
            if tests.is_some() {
                function_tests::load_test_area(&mut rcon)?;
            }
            for test in tests.iter().flat_map(|(tests, _)| tests) {
                results.push(function_tests::run(&mut rcon, &mut server.log, test)?);
            }
            text.push_str(&server.stop(&mut rcon)?);
            (text, true, results)
        }
        Started::Stopped(text) => (text, false, Vec::new()),
    };
    modstage::stop_leftovers(config, &name)?;

    let mut found = problems::find(&text, &support.pack, minecraft.data_format);
    if let Some((_, test_pack)) = tests {
        for problem in problems::find(&text, test_pack, minecraft.data_format) {
            if !found.contains(&problem) {
                found.push(problem);
            }
        }
    }
    if found.is_empty() && !ready {
        found.push(Problem {
            source: None,
            at: None,
            message: format!(
                "Minecraft stopped before it finished starting; see {}",
                log.display()
            ),
        });
    }
    Ok(Loaded {
        minecraft: minecraft.clone(),
        problems: found,
        tests: results,
        log,
    })
}
