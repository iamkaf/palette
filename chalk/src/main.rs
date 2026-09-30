use chalk::dev::DevOptions;
use chalk::pair::TestOptions;
use chalk::publish::{self, PublishMode};
use chalk::{PackRoot, TOOL_NAME, build, changes, check, dev, game, init, pair, teakit};
use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut args: Vec<String>) -> chalk::Result<bool> {
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        print_help();
        return Ok(true);
    }
    let command = args.remove(0);
    if matches!(command.as_str(), "-V" | "--version") {
        println!("{TOOL_NAME} {}", env!("CARGO_PKG_VERSION"));
        return Ok(true);
    }
    if command == "vanilla-table" {
        if args.is_empty() {
            return Err("vanilla-table needs server jars, one per data format".into());
        }
        let jars: Vec<std::path::PathBuf> = args.iter().map(Into::into).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&changes::generate(&jars)?)?
        );
        return Ok(true);
    }
    if command == "init" {
        let dir = match args.as_slice() {
            [] => env::current_dir()?,
            [dir] => env::current_dir()?.join(dir),
            _ => return Err("init takes at most one directory".into()),
        };
        init::init(&dir)?;
        println!("Created a pack in {}", dir.display());
        println!("Run `{TOOL_NAME} dev` there to play it, or `{TOOL_NAME} check` to test it.");
        return Ok(true);
    }
    let root = PackRoot::discover(&env::current_dir()?)?;
    match command.as_str() {
        "check" => {
            let options = parse_check_args(&args)?;
            let support = check::support(&root)?;
            print_support(&support);
            if !teakit::test_files(&root)?.is_empty() {
                teakit::typecheck(&root)?;
            }
            if !options.game {
                return Ok(true);
            }
            let targets = check::select(&support.minecraft, &options.minecraft)?;
            println!("Loading the pack in Minecraft");
            let mut clean = true;
            for loaded in game::load(&root, &support, &targets)? {
                clean &= game::print(&root, &loaded);
            }
            Ok(clean)
        }
        "dev" => {
            let options = parse_dev_args(&args)?;
            dev::dev(&root, &options)
        }
        "test" => {
            let options = parse_test_args(&args)?;
            let support = check::support(&root)?;
            pair::test(&root, &support, &options)
        }
        "changes" => {
            no_arguments("changes", &args)?;
            let support = check::support(&root)?;
            let table = changes::table()?;
            let found = changes::find(&support, &table)?;
            if found.is_empty() {
                println!(
                    "Nothing in the pack's JSON files changes across Minecraft {} in vanilla data",
                    support.pack.minecraft
                );
            } else {
                println!(
                    "To look into, from vanilla data across Minecraft {}",
                    support.pack.minecraft
                );
                changes::print(&root, &found);
            }
            Ok(true)
        }
        "build" => {
            no_arguments("build", &args)?;
            let support = check::support(&root)?;
            println!("{}", build::build(&root, &support.pack)?.display());
            Ok(true)
        }
        "prepare" => {
            no_arguments("prepare", &args)?;
            println!("{}", publish::prepare_release(&root)?.display());
            Ok(true)
        }
        "verify" => {
            no_arguments("verify", &args)?;
            let release = publish::verify_release(&root)?;
            println!(
                "verified {} prepared files for version {}",
                release.artifacts.len(),
                release.pack_version
            );
            Ok(true)
        }
        "publish" => {
            let mode = match args.as_slice() {
                [] => PublishMode::Publish,
                [flag] if flag == "--dry-run" => PublishMode::DryRun,
                _ => return Err("publish takes only --dry-run".into()),
            };
            for line in publish::publish(&root, mode)? {
                println!("{line}");
            }
            Ok(true)
        }
        other => Err(format!("unknown command {other}; run `{TOOL_NAME} --help`").into()),
    }
}

fn print_support(support: &check::Support) {
    println!("Minecraft {}", support.pack.minecraft);
    for minecraft in &support.minecraft {
        let variants: usize = support
            .pack
            .overlays
            .iter()
            .filter(|overlay| overlay.formats.contains(minecraft.data_format))
            .map(|overlay| overlay.files.len())
            .sum();
        match variants {
            0 => println!("  {}", minecraft.version),
            1 => println!("  {:<8} 1 variant", minecraft.version),
            count => println!("  {:<8} {count} variants", minecraft.version),
        }
    }
}

struct CheckOptions {
    minecraft: Vec<String>,
    game: bool,
}

fn parse_check_args(args: &[String]) -> chalk::Result<CheckOptions> {
    let mut options = CheckOptions {
        minecraft: Vec::new(),
        game: true,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--minecraft" => {
                let version = args.next().ok_or("--minecraft needs a version")?;
                options.minecraft.push(version.clone());
            }
            "--no-game" => options.game = false,
            other => return Err(format!("unknown check option {other}").into()),
        }
    }
    Ok(options)
}

fn parse_dev_args(args: &[String]) -> chalk::Result<DevOptions> {
    let mut options = DevOptions { minecraft: None };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--minecraft" => {
                let version = args.next().ok_or("--minecraft needs a version")?;
                options.minecraft = Some(version.clone());
            }
            other => return Err(format!("unknown dev option {other}").into()),
        }
    }
    Ok(options)
}

fn parse_test_args(args: &[String]) -> chalk::Result<TestOptions> {
    let mut options = TestOptions {
        minecraft: Vec::new(),
        visible: false,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--minecraft" => {
                let version = args.next().ok_or("--minecraft needs a version")?;
                options.minecraft.push(version.clone());
            }
            "--visible" => options.visible = true,
            other => return Err(format!("unknown test option {other}").into()),
        }
    }
    Ok(options)
}

fn no_arguments(command: &str, args: &[String]) -> chalk::Result<()> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!("{command} takes no arguments").into())
    }
}

fn print_help() {
    println!(
        "Chalk
Develop and test Minecraft datapacks across Minecraft versions

  chalk init [<dir>]             Create a pack in a new or empty directory
  chalk check                    Validate the pack, typecheck its tests, and load it in
                                 every supported Minecraft version
  chalk check --minecraft <ver>  Load it in one version; repeat for more
  chalk check --no-game          Skip loading it in Minecraft
  chalk dev                      Run a server with the pack on the newest Minecraft version
                                 and reload the pack whenever you save
  chalk dev --minecraft <ver>    Run it on another version
  chalk test                     Run the tests on every Minecraft version the pack supports
  chalk test --minecraft <ver>   Run them on one version; repeat for more
  chalk test --visible           Show the Minecraft window instead of using Xvfb (Linux)
  chalk changes                  List what the pack's JSON files use that vanilla data only
                                 uses in some of the versions reading them
  chalk build                    Build the pack into build/chalk/<slug>.zip
  chalk prepare                  Build the release files into build/chalk/dist/
  chalk verify                   Check the prepared files still match the sources
  chalk publish                  Upload the prepared files to chalk.toml's [publish] targets
  chalk publish --dry-run        Show what publishing would upload, without uploading
  chalk --version                Print the Chalk version
  chalk vanilla-table <jar>...   For Chalk maintainers: print the table `chalk changes`
                                 reads, from server jars, one per data format

Run Chalk inside a pack repository: chalk.toml, the pack's files in datapack/, and
TeaKit tests in tests/. A file named frame@-26.2.json replaces frame.json on Minecraft
26.2 and older."
    );
}
