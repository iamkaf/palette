//! Runs a vanilla server with the pack and reloads it whenever its sources change.

use crate::modstage::{self, Instance, Loader};
use crate::rcon::Rcon;
use crate::server::{Console, Log, Server, Started};
use crate::versions::Minecraft;
use crate::{PackRoot, Result, build, check, problems};
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

/// How long the log must stay quiet after `/reload` before Chalk reports the result.
const SETTLE: Duration = Duration::from_secs(1);

pub struct DevOptions {
    /// The Minecraft version to run. `None` means the newest one the pack supports.
    pub minecraft: Option<String>,
}

/// Starts the server, then rebuilds and reloads the pack on every change until the server
/// stops. Returns whether it stopped cleanly.
pub fn dev(root: &PackRoot, options: &DevOptions) -> Result<bool> {
    let support = check::support(root)?;
    let requested: Vec<String> = options.minecraft.iter().cloned().collect();
    let minecraft = check::select(&support.minecraft, &requested)?
        .into_iter()
        .next()
        .ok_or("the pack supports no Minecraft version")?;
    let name = format!("dev-{}", minecraft.version);
    let build_dir = root.build_dir();
    let staged = build_dir.join("dev").join(root.slug());
    build::unpack(&support.pack, &staged)?;

    let port = server_port()?;
    let console = Console::new()?;
    let config = build_dir.join("modstage-dev.toml");
    fs::write(
        &config,
        modstage::config(
            root.slug(),
            &[Instance {
                name: name.clone(),
                minecraft: &minecraft.version,
                loader: Loader::Vanilla,
                sides: &["server"],
                mods: Vec::new(),
                properties: [
                    // Only this computer can join or reach the console.
                    ("server-ip", "127.0.0.1".into()),
                    ("server-port", port.to_string()),
                ]
                .into_iter()
                .chain(console.properties())
                .collect(),
                packs: vec![(&staged, root.slug().to_owned())],
            }],
        ),
    )?;

    modstage::stop_leftovers(&config, &name)?;
    // Modstage copies the pack over the world's copy, so clear files the last run left.
    if let Some(dir) = world_pack(&config, &name, root.slug())?
        && dir.exists()
    {
        fs::remove_dir_all(dir)?;
    }

    fs::create_dir_all(build_dir.join("logs"))?;
    let log_path = build_dir.join("logs").join(format!("{name}.log"));
    println!(
        "Starting Minecraft {} with {}",
        minecraft.version,
        root.slug()
    );
    let mut server = Server::start(&config, &name, log_path)?;
    let startup = match server.wait_ready()? {
        Started::Ready(text) => text,
        Started::Stopped(text) => {
            problems::print(
                root,
                &problems::find(&text, &support.pack, minecraft.data_format),
            );
            println!(
                "Minecraft stopped before it finished starting; see {}",
                server.log_path.display()
            );
            return Ok(false);
        }
    };
    let found = problems::find(&startup, &support.pack, minecraft.data_format);
    problems::print(root, &found);
    let world_pack = world_pack(&config, &name, root.slug())?
        .ok_or("Modstage doesn't know where the dev server lives")?;
    let mut rcon = console.connect()?;
    println!(
        "Join 127.0.0.1:{port} from Minecraft {}. Chalk reloads the pack when you save, \
         and Ctrl+C stops the server.",
        minecraft.version
    );

    let mut seen = snapshot(root)?;
    loop {
        thread::sleep(Duration::from_millis(300));
        op_joined_players(&mut rcon, &server.log.read_new()?)?;
        if let Some(status) = server.exited()? {
            println!("Minecraft stopped");
            return Ok(status.success());
        }
        let now = snapshot(root)?;
        if now == seen {
            continue;
        }
        // Give editors a moment to finish writing every file they save together.
        thread::sleep(Duration::from_millis(200));
        seen = snapshot(root)?;
        reload(root, &minecraft, &world_pack, &mut rcon, &mut server.log)?;
    }
}

fn reload(
    root: &PackRoot,
    minecraft: &Minecraft,
    world_pack: &Path,
    rcon: &mut Rcon,
    log: &mut Log,
) -> Result<()> {
    let support = match check::support(root) {
        Ok(support) => support,
        Err(error) => {
            println!("Not reloaded: {error}");
            return Ok(());
        }
    };
    if !support.pack.formats.contains(minecraft.data_format) {
        println!(
            "Not reloaded: the pack no longer supports Minecraft {}",
            minecraft.version
        );
        return Ok(());
    }
    build::unpack(&support.pack, world_pack)?;
    log.read_new()?;
    rcon.command("reload")?;
    let text = log.read_until_quiet(SETTLE)?;
    let found = problems::find(&text, &support.pack, minecraft.data_format);
    match found.len() {
        0 => println!("Reloaded"),
        1 => println!("Reloaded with 1 problem"),
        count => println!("Reloaded with {count} problems"),
    }
    problems::print(root, &found);
    Ok(())
}

/// Makes everyone who joins an operator, so they can run commands and functions.
fn op_joined_players(rcon: &mut Rcon, log: &str) -> Result<()> {
    for player in log.lines().filter_map(joined_player) {
        rcon.command(&format!("op {player}"))?;
    }
    Ok(())
}

/// The name in a `Kaf[/127.0.0.1:50000] logged in with entity id ...` line.
fn joined_player(line: &str) -> Option<&str> {
    let (_, message) = line.split_once("]: ")?;
    let (player, rest) = message.split_once('[')?;
    let valid = !player.is_empty()
        && player
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    (valid && rest.contains("] logged in with entity id ")).then_some(player)
}

/// The pack's folder in the dev world. `None` before the server's first run.
fn world_pack(config: &Path, instance: &str, slug: &str) -> Result<Option<PathBuf>> {
    Ok(modstage::instance_dir(config, instance)?.map(|dir| {
        dir.join("server")
            .join("game")
            .join("world")
            .join("datapacks")
            .join(slug)
    }))
}

/// Minecraft's default port when it's free, so players can reuse their server entry.
fn server_port() -> Result<u16> {
    match TcpListener::bind(("127.0.0.1", 25565)) {
        Ok(_) => Ok(25565),
        Err(_) => modstage::free_port(),
    }
}

/// Every source file with its modification time and size.
fn snapshot(root: &PackRoot) -> Result<Vec<(PathBuf, SystemTime, u64)>> {
    let mut files = check::files(&root.pack_dir())?;
    files.push(root.manifest());
    let mut snapshot = Vec::new();
    for file in files {
        // A file an editor is replacing can vanish between listing and reading it.
        if let Ok(metadata) = fs::metadata(&file) {
            snapshot.push((file, metadata.modified()?, metadata.len()));
        }
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joined_players_are_read_from_the_login_line() {
        assert_eq!(
            joined_player(
                "[13:30:01] [Server thread/INFO]: Kaf_2[/127.0.0.1:51234] logged in with entity id 42 at (0.5, 64.0, 0.5)"
            ),
            Some("Kaf_2")
        );
        assert_eq!(
            joined_player("[13:30:01] [Server thread/INFO]: <Kaf> x[y] logged in with entity id 1"),
            None
        );
        assert_eq!(
            joined_player("[13:30:01] [Server thread/INFO]: Kaf joined the game"),
            None
        );
    }
}
