//! Loads the pack in a vanilla dedicated server for each Minecraft version and reports
//! what the game said about it.

use crate::check::Support;
use crate::modstage::{self, Instance, Loader};
use crate::problems::{self, Problem};
use crate::versions::Minecraft;
use crate::{PackRoot, Result, build};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;

/// What happened when one Minecraft version loaded the pack.
pub struct Loaded {
    pub minecraft: Minecraft,
    pub problems: Vec<Problem>,
    pub log: PathBuf,
}

/// Boots a vanilla server with the built zip for each target, a few at a time, and
/// returns results in the order of `targets`.
pub fn load(root: &PackRoot, support: &Support, targets: &[Minecraft]) -> Result<Vec<Loaded>> {
    let archive = build::build(root, &support.pack)?;
    let dir = root.build_dir();
    fs::create_dir_all(dir.join("logs"))?;
    let config = dir.join("modstage-load.toml");
    let mut instances = Vec::new();
    for minecraft in targets {
        instances.push(Instance {
            name: instance(minecraft),
            minecraft: &minecraft.version,
            loader: Loader::Vanilla,
            sides: &["server"],
            mods: Vec::new(),
            // Several servers run at once, so each needs its own port.
            properties: vec![("server-port", modstage::free_port()?.to_string())],
            pack: &archive,
            pack_name: format!("{}.zip", root.slug()),
        });
    }
    fs::write(&config, modstage::config(root.slug(), &instances))?;

    let pending = Mutex::new(targets.iter().collect::<Vec<_>>());
    let results = Mutex::new(Vec::new());
    let workers = thread::available_parallelism()
        .map_or(1, |cores| cores.get() / 4)
        .clamp(1, 3)
        .min(targets.len());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while let Some(minecraft) =
                    pending.lock().ok().and_then(|mut pending| pending.pop())
                {
                    let loaded = load_one(root, support, &config, minecraft);
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

fn instance(minecraft: &Minecraft) -> String {
    format!("load-{}", minecraft.version)
}

fn load_one(
    root: &PackRoot,
    support: &Support,
    config: &Path,
    minecraft: &Minecraft,
) -> Result<Loaded> {
    let name = instance(minecraft);
    let log = root.build_dir().join("logs").join(format!("{name}.log"));
    modstage::stop_leftovers(config, &name)?;
    let output = File::create(&log)?;
    // Without --keep-alive, Modstage starts the server, waits until it is ready, and stops it.
    Command::new("modstage")
        .arg("--config")
        .arg(config)
        .args(["run", "server", &name, "--timeout", "300s"])
        .stdout(output.try_clone()?)
        .stderr(output)
        .stdin(Stdio::null())
        .status()
        .map_err(|error| format!("running modstage: {error}"))?;
    modstage::stop_leftovers(config, &name)?;

    let text = fs::read_to_string(&log)?;
    let mut found = problems::find(&text, &support.pack, minecraft.data_format);
    if found.is_empty() && !text.contains("Done (") {
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
        log,
    })
}
