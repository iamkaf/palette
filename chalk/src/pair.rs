use crate::check::{self, Support};
use crate::modstage::{self, Instance, Loader};
use crate::versions::Minecraft;
use crate::{PackRoot, Result, build, problems, teakit};
use serde_json::Value;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use toml::Table;

pub struct TestOptions {
    /// Minecraft versions to test. Empty means every version the pack supports.
    pub minecraft: Vec<String>,
    /// Show the Minecraft window instead of running through Xvfb.
    pub visible: bool,
}

/// Runs every test file on each selected Minecraft version, in a Fabric client connected
/// to a Fabric dedicated server that loads the pack. Returns whether everything passed.
pub fn test(root: &PackRoot, support: &Support, options: &TestOptions) -> Result<bool> {
    let targets = check::select(&support.minecraft, &options.minecraft)?;
    let tests = teakit::test_files(root)?;
    if tests.is_empty() {
        return Err(format!("no *.test.ts files in {}", root.tests_dir().display()).into());
    }
    // Only Linux can hide the window, through a virtual X server.
    let headless_run = !options.visible && cfg!(target_os = "linux");
    if headless_run && !on_path("xvfb-run") {
        return Err(
            "xvfb-run is needed to run Minecraft in the background; install it or pass --visible"
                .into(),
        );
    }

    // Test the archive players install, not the source folder.
    let archive = build::build(root, &support.pack)?;
    let build = root.build_dir();
    fs::create_dir_all(build.join("reports"))?;
    fs::create_dir_all(build.join("logs"))?;
    let modstage = build.join("modstage.toml");
    let teakit = build.join("teakit.toml");
    let instances: Vec<Instance<'_>> = targets
        .iter()
        .map(|minecraft| Instance {
            name: instance_id(minecraft),
            minecraft: &minecraft.version,
            loader: Loader::Fabric(&support.versions.fabric_loader),
            sides: &["client", "server"],
            mods: std::iter::once(format!(
                "maven:com.iamkaf.teakit:teakit-fabric:{}",
                minecraft.teakit
            ))
            .chain(
                minecraft
                    .mods
                    .iter()
                    .map(|entry| format!("modrinth:{entry}")),
            )
            .collect(),
            properties: Vec::new(),
            pack: &archive,
            pack_name: format!("{}.zip", root.slug()),
        })
        .collect();
    fs::write(&modstage, modstage::config(root.slug(), &instances))?;
    fs::write(&teakit, teakit_config(root, &targets)?)?;

    let mut passed = true;
    for minecraft in &targets {
        for test in &tests {
            let name = test_name(test);
            let run = format!("{}-{name}", minecraft.version);
            let report = build.join("reports").join(format!("{run}.json"));
            let log = build.join("logs").join(format!("{run}.log"));
            let _ = fs::remove_file(&report);
            println!("{} {name}", minecraft.version);
            modstage::stop_leftovers(&modstage, &instance_id(minecraft))?;

            let mut pair = teakit::command(root)?;
            pair.arg("pair")
                .arg("--no-sync-sdk")
                .arg("--config")
                .arg(&teakit)
                .arg("--node")
                .arg(node(minecraft))
                .arg("--modstage-config")
                .arg(&modstage)
                .arg("--modstage-instance")
                .arg(instance_id(minecraft))
                .arg("--test-file")
                .arg(test)
                .arg("--timeout")
                .arg("600")
                .arg("--report")
                .arg(&report);
            let mut command = if headless_run { headless(pair) } else { pair };
            let server_log = server_log(root, minecraft, test);
            let _ = fs::remove_file(&server_log);
            let output = File::create(&log)?;
            command
                .stdout(output.try_clone()?)
                .stderr(output)
                .stdin(Stdio::null());
            command
                .status()
                .map_err(|error| format!("starting the TeaKit pair: {error}"))?;

            passed &= summarize(&report, &log);
            let found = fs::read_to_string(&server_log)
                .map(|log| problems::find(&log, &support.pack, minecraft.data_format))
                .unwrap_or_default();
            problems::print(root, &found);
            passed &= found.is_empty();
            modstage::stop_leftovers(&modstage, &instance_id(minecraft))?;
        }
    }
    Ok(passed)
}

fn test_name(path: &Path) -> String {
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    file.trim_end_matches(".test.ts").to_owned()
}

fn node(minecraft: &Minecraft) -> String {
    format!("{}-fabric", minecraft.version)
}

fn instance_id(minecraft: &Minecraft) -> String {
    format!("pair-{}", minecraft.version)
}

fn teakit_config(root: &PackRoot, targets: &[Minecraft]) -> Result<String> {
    let mut nodes = Table::new();
    for minecraft in targets {
        let mut node_table = Table::new();
        node_table.insert("loader".into(), "fabric".into());
        node_table.insert("minecraft".into(), minecraft.version.clone().into());
        nodes.insert(node(minecraft), node_table.into());
    }
    let mut config = Table::new();
    config.insert("modId".into(), root.slug().into());
    config.insert("projectKey".into(), root.slug().into());
    config.insert("nodes".into(), nodes.into());
    Ok(toml::to_string(&config)?)
}

// xvfb-run takes the X server's options as one argument.
#[allow(clippy::suspicious_command_arg_space)]
fn headless(pair: Command) -> Command {
    let mut command = Command::new("xvfb-run");
    command
        .arg("-a")
        .arg("-s")
        .arg("-screen 0 1920x1080x24")
        .arg(pair.get_program())
        .args(pair.get_args());
    if let Some(dir) = pair.get_current_dir() {
        command.current_dir(dir);
    }
    command
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

/// Where the TeaKit runner keeps the dedicated server's log for a test run.
fn server_log(root: &PackRoot, minecraft: &Minecraft, test: &Path) -> PathBuf {
    let file = test
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    root.dir()
        .join("build")
        .join("teakit")
        .join(node(minecraft))
        .join(format!("{file}.pair"))
        .join("modstage-server.log")
}

/// Prints one line per test from the runner's report and returns whether all passed.
fn summarize(report: &Path, log: &Path) -> bool {
    let parsed = fs::read_to_string(report)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let Some(report) = parsed else {
        println!("  failed before reporting; see {}", log.display());
        return false;
    };
    let tests = report.pointer("/result/tests").and_then(Value::as_array);
    let Some(tests) = tests.filter(|tests| !tests.is_empty()) else {
        let error = report
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("no tests ran");
        println!("  failed: {}", one_line(error));
        println!("  log: {}", log.display());
        return false;
    };
    let mut passed = true;
    for test in tests {
        let name = test
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unnamed test");
        match test.get("status").and_then(Value::as_str) {
            Some("passed") => println!("  ok      {name}"),
            Some("skipped") => println!("  skipped {name}"),
            status => {
                passed = false;
                let message = test
                    .pointer("/failure/message")
                    .and_then(Value::as_str)
                    .unwrap_or(status.unwrap_or("unknown"));
                println!("  FAILED  {name}: {}", one_line(message));
            }
        }
    }
    if !passed {
        println!("  log: {}", log.display());
    }
    passed
}

/// Collapses a multi-line error, such as a JSON body, onto one summary line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
