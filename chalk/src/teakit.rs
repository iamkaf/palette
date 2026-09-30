use crate::{PackRoot, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// TeaKit's wrapper script. It downloads and caches the pinned TeaKit runner.
#[cfg(not(windows))]
const TEAKITW: (&str, &str) = ("teakitw", include_str!("teakitw"));
#[cfg(windows)]
const TEAKITW: (&str, &str) = ("teakitw.bat", include_str!("teakitw.bat"));

/// Writes the wrapper into `build/chalk/` and returns a command that runs it from the
/// repository root.
pub fn command(root: &PackRoot) -> Result<Command> {
    let path = wrapper(root)?;
    let mut command = Command::new(path);
    command.current_dir(root.dir());
    Ok(command)
}

fn wrapper(root: &PackRoot) -> Result<PathBuf> {
    let dir = root.build_dir();
    fs::create_dir_all(&dir)?;
    let (name, script) = TEAKITW;
    let path = dir.join(name);
    fs::write(&path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(path)
}

/// Installs the `@teakit/test` types and typechecks every test file.
pub fn typecheck(root: &PackRoot) -> Result<()> {
    let sdk = root.build_dir().join("sdk");
    run(
        command(root)?
            .arg("install-sdk")
            .arg("--target-dir")
            .arg(&sdk),
        "installing the TeaKit SDK",
    )?;

    let types = "./sdk/.teakit/types/@teakit/test";
    let tsconfig = serde_json::json!({
        "compilerOptions": {
            "paths": {
                "@teakit/test": [types],
                "@teakit/test/protocol": [format!("{types}/protocol.d.ts")],
                "@teakit/test/capabilities.json": [format!("{types}/capabilities.json")]
            },
            "resolveJsonModule": true,
            "module": "ESNext",
            "moduleResolution": "Bundler",
            "noEmit": true,
            "strict": true,
            "target": "ES2022"
        },
        "include": [root.tests_dir().join("*.test.ts")]
    });
    let tsconfig_path = root.build_dir().join("tsconfig.json");
    fs::write(&tsconfig_path, serde_json::to_string_pretty(&tsconfig)?)?;
    run(
        command(root)?
            .arg("typecheck")
            .arg("--no-sync-sdk")
            .arg("--tsconfig")
            .arg(&tsconfig_path)
            .arg("--timeout")
            .arg("120"),
        "typechecking the tests",
    )
}

fn run(command: &mut Command, action: &str) -> Result<()> {
    let status = command
        .status()
        .map_err(|error| format!("{action}: {error}"))?;
    if !status.success() {
        return Err(format!("{action} failed ({status})").into());
    }
    Ok(())
}

/// Every `*.test.ts` file in `tests/`, sorted.
pub fn test_files(root: &PackRoot) -> Result<Vec<PathBuf>> {
    let dir = root.tests_dir();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut tests: Vec<PathBuf> = fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.to_string_lossy().ends_with(".test.ts"))
        .collect();
    tests.sort();
    Ok(tests)
}
