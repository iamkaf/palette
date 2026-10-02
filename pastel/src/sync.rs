//! Reconciles a server folder with a pack.

use crate::pack::{self, LaunchTarget, LoadedMrpack, Manifest};
use crate::state::{self, State};
use crate::ui::SyncReport;
use crate::{Context, Error, Result, fetch, http, paths};
use chrono::Utc;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// How many files download at once.
const DOWNLOAD_WORKERS: usize = 8;

pub struct Options<'a> {
    pub root: &'a Path,
    pub manifest: &'a mut Manifest,
    pub pack_coordinate: &'a str,
    pub mrpack: &'a LoadedMrpack,
    pub prune_mods: bool,
    pub dry_run: bool,
    pub report: &'a mut SyncReport,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub downloaded: usize,
    pub unchanged: usize,
    pub overrides: usize,
    /// Whether a loader launcher was installed this run.
    pub loader: bool,
    pub pruned: Vec<String>,
}

pub fn run(options: Options<'_>) -> Result<Outcome> {
    let Options {
        root,
        manifest,
        pack_coordinate,
        mrpack,
        prune_mods,
        dry_run,
        report,
    } = options;
    manifest.validate()?;
    let root = paths::absolute(root)?;
    fs::create_dir_all(&root)?;

    let mut outcome = Outcome::default();
    let mut wanted_mods = HashSet::new();
    let mut wanted_root_jars = HashSet::new();
    let mut jobs = Vec::new();
    for file in &manifest.files {
        let dest = paths::join_slash(&root, &file.path);
        let name = file
            .path
            .rsplit('/')
            .next()
            .unwrap_or(&file.path)
            .to_owned();
        if file.path.starts_with("mods/") {
            wanted_mods.insert(name.clone());
        }
        // Root-level jars, such as a launcher the pack ships, are kept by pruning.
        if !file.path.contains('/') && file.path.to_lowercase().ends_with(".jar") {
            wanted_root_jars.insert(name);
        }
        if !dry_run {
            jobs.push((file, dest));
            continue;
        }
        let matches = dest.is_file()
            && file.preferred_hash().is_ok_and(|(algorithm, want)| {
                fetch::file_matches(&dest, algorithm, want).unwrap_or(false)
            });
        if matches {
            outcome.unchanged += 1;
            report.unchanged(&file.path);
        } else if dest.is_file() {
            outcome.downloaded += 1;
            report.would_update(&file.path);
        } else {
            outcome.downloaded += 1;
            report.would_download(&file.path);
        }
    }
    if !jobs.is_empty() {
        download_parallel(&jobs, report, &mut outcome)?;
    }

    if dry_run {
        outcome.overrides += 1;
        report.would_update("overrides");
        // Keep what a real run keeps, so the preview's pruning matches it: jars
        // from overrides, and the launcher of a loader that's already installed.
        if let Ok(names) = mrpack.list_override_mod_jars() {
            wanted_mods.extend(names);
        }
        if let Some(jar) = pack::installed_launch_jar(&root, manifest) {
            wanted_root_jars.insert(jar);
        }
    } else {
        let written = mrpack.apply_overrides(&root).context("mrpack overrides")?;
        if !written.is_empty() {
            outcome.overrides = written.len();
            report.download(&format!("overrides ({} files)", written.len()));
        }
        // Jars shipped only through overrides/mods/ are not "extra".
        wanted_mods.extend(pack::override_mod_jars(&written));

        outcome.loader = pack::ensure_loader(&root, manifest).context("loader")?;
        if let Some(LaunchTarget::Jar(jar)) = manifest.launch.as_ref().map(|launch| &launch.target)
        {
            if outcome.loader {
                report.download(jar);
            }
            wanted_root_jars.insert(jar.rsplit('/').next().unwrap_or(jar).to_owned());
        }
    }

    if prune_mods {
        // A pack without mods leaves mods/ alone.
        if !wanted_mods.is_empty() {
            prune(
                &root.join("mods"),
                "mods/",
                |name| name.to_lowercase().ends_with(".jar") && !wanted_mods.contains(name),
                dry_run,
                report,
                &mut outcome,
            )?;
        }
        // Old Fabric, Forge, NeoForge, Quilt, and vanilla launchers left after upgrades.
        prune(
            &root,
            "",
            |name| pack::is_managed_root_jar(name) && !wanted_root_jars.contains(name),
            dry_run,
            report,
            &mut outcome,
        )?;
    }
    report.flush();

    if !dry_run {
        state::save(
            &root,
            &State {
                pack_coordinate: pack_coordinate.to_owned(),
                pack_name: manifest.name.clone(),
                pack_version: manifest.version.clone(),
                minecraft: manifest.minecraft().to_owned(),
                loader: manifest.loader_name().to_owned(),
                mod_count: manifest.mod_count(),
                applied_at: Utc::now(),
                file_count: manifest.files.len() + outcome.overrides,
            },
        )
        .context("save state")?;
    }
    Ok(outcome)
}

fn download_parallel(
    jobs: &[(&pack::PackFile, PathBuf)],
    report: &mut SyncReport,
    outcome: &mut Outcome,
) -> Result<()> {
    let client = http::client(Duration::from_secs(600))?;
    let next = AtomicUsize::new(0);
    let shared = Mutex::new((report, outcome, None::<Error>));
    std::thread::scope(|scope| {
        for _ in 0..DOWNLOAD_WORKERS.min(jobs.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((file, dest)) = jobs.get(index) else {
                        return;
                    };
                    if lock(&shared).2.is_some() {
                        return;
                    }
                    let result = fetch::ensure_file(&client, file, dest);
                    let mut guard = lock(&shared);
                    let (report, outcome, first_error) = &mut *guard;
                    if first_error.is_some() {
                        return;
                    }
                    match result {
                        Ok(true) => {
                            outcome.downloaded += 1;
                            report.download(&file.path);
                        }
                        Ok(false) => {
                            outcome.unchanged += 1;
                            report.unchanged(&file.path);
                        }
                        Err(error) => *first_error = Some(error),
                    }
                }
            });
        }
    });
    let (_, _, first_error) = shared
        .into_inner()
        .unwrap_or_else(|error| error.into_inner());
    first_error.map_or(Ok(()), Err)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// Removes files in `dir` that `extra` selects, recording them under `prefix`.
fn prune(
    dir: &Path,
    prefix: &str,
    extra: impl Fn(&str) -> bool,
    dry_run: bool,
    report: &mut SyncReport,
    outcome: &mut Outcome,
) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| !kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| extra(name))
        .collect();
    names.sort();
    for name in names {
        let rel = format!("{prefix}{name}");
        if dry_run {
            report.would_prune(&rel);
        } else {
            fs::remove_file(dir.join(&name)).context(format_args!("prune {name}"))?;
            report.prune(&rel);
        }
        outcome.pruned.push(rel);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn local_pack(dir: &Path) -> pack::Resolved {
        let path = dir.join("pack.mrpack");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("modrinth.index.json", options).unwrap();
        zip.write_all(
            br#"{"formatVersion": 1, "versionId": "1.0.0", "name": "Vanilla Pack",
                 "dependencies": {"minecraft": "1.21.1"}}"#,
        )
        .unwrap();
        for (name, body) in [
            ("overrides/server.jar", "jar"),
            ("server-overrides/mods/kept.jar", "jar"),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        pack::resolve(path.to_str().unwrap(), &[], None).unwrap()
    }

    #[test]
    fn previews_keep_the_installed_launcher() {
        let root = tempfile::tempdir().unwrap();
        let current = "fabric-server-mc.26.2-loader.0.19.3-launcher.1.1.2.jar";
        let stale = "fabric-server-mc.26.1.2-loader.0.19.2-launcher.1.1.1.jar";
        fs::write(root.path().join(current), "jar").unwrap();
        fs::write(root.path().join(stale), "jar").unwrap();
        let mut manifest = Manifest {
            name: "Fabric Pack".to_owned(),
            version: "1.0.0".to_owned(),
            dependencies: [("minecraft", "26.2"), ("fabric-loader", "0.19.3")]
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .into(),
            ..Manifest::default()
        };
        let mrpack =
            LoadedMrpack::from_bytes(br#"{"versionId": "1.0.0", "name": "Fabric Pack"}"#).unwrap();
        let preview = run(Options {
            root: root.path(),
            manifest: &mut manifest,
            pack_coordinate: "file:pack.mrpack",
            mrpack: &mrpack,
            prune_mods: true,
            dry_run: true,
            report: &mut SyncReport::new(false),
        })
        .unwrap();
        assert_eq!(preview.pruned, [stale]);
    }

    #[test]
    fn prunes_extra_mods_but_keeps_override_jars() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("server");
        fs::create_dir_all(root.join("mods")).unwrap();
        fs::write(root.join("mods/extra.jar"), "x").unwrap();
        fs::write(root.join("mods/notes.txt"), "x").unwrap();
        fs::write(
            root.join("fabric-server-mc.1.21-loader.0.1-launcher.1.0.jar"),
            "x",
        )
        .unwrap();
        let mut resolved = local_pack(dir.path());

        let mut report = SyncReport::new(false);
        let preview = run(Options {
            root: &root,
            manifest: &mut resolved.manifest,
            pack_coordinate: &resolved.coordinate,
            mrpack: &resolved.mrpack,
            prune_mods: true,
            dry_run: true,
            report: &mut report,
        })
        .unwrap();
        assert_eq!(
            preview.pruned,
            [
                "mods/extra.jar",
                "fabric-server-mc.1.21-loader.0.1-launcher.1.0.jar"
            ]
        );
        assert!(root.join("mods/extra.jar").exists());
        assert!(state::load(&root).unwrap().is_none());

        let outcome = run(Options {
            root: &root,
            manifest: &mut resolved.manifest,
            pack_coordinate: &resolved.coordinate,
            mrpack: &resolved.mrpack,
            prune_mods: true,
            dry_run: false,
            report: &mut report,
        })
        .unwrap();
        assert_eq!(outcome.pruned, preview.pruned);
        assert!(root.join("mods/kept.jar").is_file());
        assert!(root.join("mods/notes.txt").is_file());
        assert!(root.join("server.jar").is_file());
        assert!(!root.join("mods/extra.jar").exists());
        let state = state::load(&root).unwrap().unwrap();
        assert_eq!(state.pack_version, "1.0.0");
        assert_eq!(state.file_count, 2);
    }
}
