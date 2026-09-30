use chalk::pair::TestOptions;
use chalk::{PackRoot, TOOL_NAME, build, check, pair, teakit};
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
    let root = PackRoot::discover(&env::current_dir()?)?;
    match command.as_str() {
        "check" => {
            no_arguments("check", &args)?;
            let support = check::support(&root)?;
            print_support(&support);
            teakit::typecheck(&root)?;
            Ok(true)
        }
        "test" => {
            let options = parse_test_args(&args)?;
            let support = check::support(&root)?;
            pair::test(&root, &support, &options)
        }
        "build" => {
            no_arguments("build", &args)?;
            let support = check::support(&root)?;
            println!("{}", build::build(&root, &support.pack)?.display());
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

  chalk check                    Validate the pack and typecheck its tests
  chalk test                     Run the tests on every Minecraft version the pack supports
  chalk test --minecraft <ver>   Run them on one version; repeat for more
  chalk test --visible           Show the Minecraft window instead of using Xvfb (Linux)
  chalk build                    Build the pack into build/chalk/<slug>.zip
  chalk --version                Print the Chalk version

Run Chalk inside a pack repository: chalk.toml, the pack's files in datapack/, and
TeaKit tests in tests/. A file named frame@-26.2.json replaces frame.json on Minecraft
26.2 and older."
    );
}
