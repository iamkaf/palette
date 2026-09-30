use crate::check::Support;
use crate::versions::Minecraft;
use crate::{PackRoot, Result, build, teakit};
use serde_json::Value;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use toml::Table;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item};

pub struct TestOptions {
    /// Minecraft versions to test. Empty means every version the pack supports.
    pub minecraft: Vec<String>,
    /// Show the Minecraft window instead of running through Xvfb.
    pub visible: bool,
}

/// Runs every test file on each selected Minecraft version, in a Fabric client connected
/// to a Fabric dedicated server that loads the pack. Returns whether everything passed.
pub fn test(root: &PackRoot, support: &Support, options: &TestOptions) -> Result<bool> {
    let targets = select(&support.minecraft, &options.minecraft)?;
    let tests = test_files(root)?;
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
    let namespaces = support.pack.namespaces();
    let build = root.build_dir();
    fs::create_dir_all(build.join("reports"))?;
    fs::create_dir_all(build.join("logs"))?;
    let modstage = build.join("modstage.toml");
    let teakit = build.join("teakit.toml");
    fs::write(
        &modstage,
        modstage_config(root, &archive, &support.versions.fabric_loader, &targets)?,
    )?;
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
            stop_leftovers(&modstage, &instance_id(minecraft))?;

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
            passed &= report_pack_errors(&server_log, &namespaces);
            stop_leftovers(&modstage, &instance_id(minecraft))?;
        }
    }
    Ok(passed)
}

/// Stops Minecraft processes still running in an instance. A dedicated server can
/// outlive its launcher when its shutdown hangs, as 1.21.1 sometimes does while saving
/// chunks, and it keeps the world locked so the next run can't start.
fn stop_leftovers(modstage: &Path, instance: &str) -> Result<()> {
    let Some(dir) = instance_dir(modstage, instance)? else {
        return Ok(());
    };
    for pid in processes_in(&dir) {
        println!("  stopping Minecraft process {pid}, which outlived its test run");
        signal(pid, "TERM");
        for _ in 0..20 {
            if !alive(pid) {
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
        if alive(pid) {
            signal(pid, "KILL");
        }
    }
    Ok(())
}

/// Asks Modstage where an instance lives. `None` before its first run.
fn instance_dir(modstage: &Path, instance: &str) -> Result<Option<PathBuf>> {
    let output = Command::new("modstage")
        .arg("--config")
        .arg(modstage)
        .args(["inspect", "instance", instance])
        .output()
        .map_err(|error| format!("running modstage: {error}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .find_map(|line| line.strip_prefix("instance_dir = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .map(PathBuf::from))
}

fn processes_in(dir: &Path) -> Vec<u32> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != std::process::id())
        .filter(|pid| {
            fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| cwd.starts_with(dir))
        })
        .collect()
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn signal(pid: u32, name: &str) {
    let _ = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(pid.to_string())
        .status();
}

fn select(supported: &[Minecraft], requested: &[String]) -> Result<Vec<Minecraft>> {
    for version in requested {
        if !supported
            .iter()
            .any(|minecraft| &minecraft.version == version)
        {
            let known: Vec<&str> = supported
                .iter()
                .map(|minecraft| minecraft.version.as_str())
                .collect();
            return Err(format!(
                "the pack doesn't support {version}; it supports {}",
                known.join(", ")
            )
            .into());
        }
    }
    Ok(supported
        .iter()
        .filter(|minecraft| requested.is_empty() || requested.contains(&minecraft.version))
        .cloned()
        .collect())
}

fn test_files(root: &PackRoot) -> Result<Vec<PathBuf>> {
    let dir = root.tests_dir();
    let mut tests: Vec<PathBuf> = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.to_string_lossy().ends_with(".test.ts"))
            .collect(),
        Err(_) => Vec::new(),
    };
    if tests.is_empty() {
        return Err(format!("no *.test.ts files in {}", dir.display()).into());
    }
    tests.sort();
    Ok(tests)
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

fn modstage_config(
    root: &PackRoot,
    archive: &Path,
    fabric_loader: &str,
    targets: &[Minecraft],
) -> Result<String> {
    let mut document = DocumentMut::new();

    let mut project = toml_edit::Table::new();
    project.insert("name", toml_edit::value(root.slug()));
    document.insert("project", Item::Table(project));

    let mut repositories = toml_edit::Table::new();
    repositories.insert("mavenLocal", toml_edit::value("mavenLocal"));
    repositories.insert("kaf", toml_edit::value("https://maven.kaf.sh"));
    document.insert("repositories", Item::Table(repositories));

    // Modstage only reads server_properties as an inline table.
    let mut server_properties = InlineTable::new();
    for (key, value) in [
        ("online-mode", "false"),
        ("enforce-secure-profile", "false"),
        // Minecraft 26.3 turns the whitelist on by default.
        ("white-list", "false"),
        ("gamemode", "creative"),
        ("spawn-protection", "0"),
    ] {
        server_properties.insert(key, value.into());
    }

    let mut instances = ArrayOfTables::new();
    for minecraft in targets {
        let mut mods = Array::new();
        mods.push(format!(
            "maven:com.iamkaf.teakit:teakit-fabric:{}",
            minecraft.teakit
        ));
        for entry in &minecraft.mods {
            mods.push(format!("modrinth:{entry}"));
        }
        let mut sides = Array::new();
        sides.push("client");
        sides.push("server");

        let mut fixture = toml_edit::Table::new();
        fixture.insert(
            "from",
            toml_edit::value(archive.to_string_lossy().into_owned()),
        );
        fixture.insert(
            "to",
            toml_edit::value(format!("world/datapacks/{}.zip", root.slug())),
        );
        fixture.insert("side", toml_edit::value("server"));
        fixture.insert("replace", toml_edit::value(true));
        let mut fixtures = ArrayOfTables::new();
        fixtures.push(fixture);

        let mut instance = toml_edit::Table::new();
        instance.insert("name", toml_edit::value(instance_id(minecraft)));
        instance.insert("minecraft", toml_edit::value(minecraft.version.as_str()));
        instance.insert("loader", toml_edit::value("fabric"));
        instance.insert("loader_version", toml_edit::value(fabric_loader));
        instance.insert("sides", toml_edit::value(sides));
        instance.insert(
            "server_properties",
            toml_edit::value(server_properties.clone()),
        );
        instance.insert("mods", toml_edit::value(mods));
        instance.insert("fixture", Item::ArrayOfTables(fixtures));
        instances.push(instance);
    }
    document.insert("instance", Item::ArrayOfTables(instances));
    Ok(document.to_string())
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

/// Prints warnings and errors the server logged about the pack's own namespaces, such as
/// a file that failed to parse, and returns whether there were none. A test can pass
/// without touching a broken file, so these fail the run on their own.
fn report_pack_errors(server_log: &Path, namespaces: &[String]) -> bool {
    let Ok(text) = fs::read_to_string(server_log) else {
        return true;
    };
    let errors = pack_errors(&text, namespaces);
    for line in errors.iter().take(10) {
        println!("  PACK    {line}");
    }
    if errors.len() > 10 {
        println!(
            "  PACK    and {} more in {}",
            errors.len() - 10,
            server_log.display()
        );
    }
    errors.is_empty()
}

fn pack_errors<'a>(log: &'a str, namespaces: &[String]) -> Vec<&'a str> {
    log.lines()
        .filter(|line| line.contains("/ERROR]") || line.contains("/WARN]"))
        .filter(|line| {
            namespaces
                .iter()
                .any(|namespace| line.contains(&format!("{namespace}:")))
        })
        .map(str::trim)
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::versions;

    #[test]
    fn pack_errors_are_warnings_and_errors_about_the_pack() {
        let log = "\
[10:00:00] [Server thread/INFO]: Loaded 12 advancements from demo:start
[10:00:00] [Server thread/ERROR]: Couldn't parse element demo:light_portal: No key trigger
[10:00:00] [Server thread/WARN]: Missing data pack file/demo
[10:00:00] [Server thread/ERROR]: Couldn't load other:thing";
        let errors = pack_errors(log, &["demo".to_owned()]);
        assert_eq!(
            errors,
            vec![
                "[10:00:00] [Server thread/ERROR]: Couldn't parse element demo:light_portal: No key trigger"
            ]
        );
    }

    #[test]
    fn server_properties_are_an_inline_table_so_modstage_reads_them() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("my-pack");
        fs::create_dir_all(repo.join("datapack")).expect("pack dir");
        let root = PackRoot::at(&repo).expect("root");
        let versions = versions::versions().expect("versions");

        let config: DocumentMut = modstage_config(
            &root,
            Path::new("my-pack.zip"),
            &versions.fabric_loader,
            &versions.minecraft,
        )
        .expect("config")
        .parse()
        .expect("valid TOML");

        let properties = config["instance"][0]["server_properties"]
            .as_inline_table()
            .expect("inline server_properties");
        assert_eq!(
            properties
                .get("white-list")
                .and_then(|value| value.as_str()),
            Some("false")
        );
    }
}
