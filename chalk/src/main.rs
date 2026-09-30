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
            check::support(&root)?;
            println!("{}", build::build(&root)?.display());
            Ok(true)
        }
        other => Err(format!("unknown command {other}; run `{TOOL_NAME} --help`").into()),
    }
}

fn print_support(support: &check::Support) {
    println!("pack.mcmeta covers formats {}", support.meta.formats);
    for minecraft in &support.minecraft {
        let overlays = support.meta.overlays_for(minecraft.data_format);
        if overlays.is_empty() {
            println!(
                "  {:<8} format {}",
                minecraft.version, minecraft.data_format
            );
        } else {
            println!(
                "  {:<8} format {}  + {}",
                minecraft.version,
                minecraft.data_format,
                overlays.join(", ")
            );
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
  chalk test --visible           Show the Minecraft window instead of using Xvfb
  chalk build                    Zip the pack into build/chalk/<slug>.zip
  chalk --version                Print the Chalk version

Run Chalk inside a pack repository: the pack in datapack/, TeaKit tests in tests/."
    );
}
