# Pastel

Pastel turns a Modrinth modpack into a managed dedicated server. It downloads and verifies the server-side pack files, installs the required loader and Java runtime, starts Minecraft, exposes a live console, restarts crashed background servers, and keeps the server on the pack version you chose.

- Restarts a background server five seconds after an unexpected crash; set `auto_restart = false` when another process manager should own restarts.
- Updates itself with `pastel self-update`, checking the release archive against its published SHA-256 before replacing the executable.
- Keeps clean shutdowns and startup failures stopped, so broken packs do not enter restart loops.

## Install

Download the archive for your computer from [Palette's GitHub Releases](https://github.com/iamkaf/palette/releases), where Pastel releases are tagged `pastel-v<version>`. Extract it into an empty server folder.

| System | Release archive |
| --- | --- |
| macOS, Apple silicon | `pastel-macos-aarch64.tar.gz` |
| macOS, Intel | `pastel-macos-x86_64.tar.gz` |
| Linux, x86-64 | `pastel-linux-x86_64.tar.gz` |
| Linux, ARM64 | `pastel-linux-aarch64.tar.gz` |
| Windows, x86-64 | `pastel-windows-x86_64.zip` |

Each release includes `release-manifest.json` with SHA-256 and SHA-512 hashes and a keyless Sigstore bundle, and GitHub records an artifact attestation for every archive. On macOS or Linux, make the executable runnable if your extraction tool did not keep its mode:

```bash
chmod +x pastel
```

To build from source with stable Rust, from the root of this repository:

```bash
cargo build --release --locked -p pastel
./target/release/pastel help
```

### Coming from Pastel 0.1

Server folders set up by Pastel 0.1 keep working as they are: the commands, `server.pastel`, and everything under `.pastel/` are unchanged, and the new executable can stop or attach to a server the old one started. Pastel 0.1 updated itself from the old `iamkaf/pastel` repository, so replace its executable with one from Palette's releases once; `pastel self-update` follows Palette from then on.

## Quick start

From the folder that should become the server:

```bash
./pastel install aristea
./pastel run
./pastel console
```

`install` accepts a Modrinth slug, a Modrinth modpack page, a direct HTTPS `.mrpack` URL, a local `.mrpack`, or a Maven coordinate with an explicit HTTPS repository:

```bash
./pastel install aristea@0.1.4
./pastel install https://modrinth.com/modpack/aristea
./pastel install https://example.com/my-pack.mrpack
./pastel install ./my-pack.mrpack
./pastel install com.example:my-pack:1.2.0 -repo https://maven.example.com
```

Pastel writes `server.pastel` with the exact pack version, applies the pack, and prepares the loader. Installing a Modrinth slug without a version pins its latest release; `pastel update` moves the pin later. The first `run` also chooses a suitable Java runtime. If the system Java is too old, Pastel downloads a Temurin JRE into `.pastel/jre/` and checks its published SHA-256 before installing it.

By running a Minecraft server with Pastel, you indicate your agreement to [Mojang's Minecraft EULA](https://aka.ms/MinecraftEULA). Pastel writes `eula=true` when it starts the server.

## Everyday commands

| Command | Purpose |
| --- | --- |
| `pastel` | Show the installed pack, server state, and available update |
| `pastel install <pack>` | Create or replace this server's pack pin |
| `pastel refresh` | Reconcile the server with the current pin |
| `pastel refresh -dry-run` | Preview downloads, updates, and pruning |
| `pastel update` | Choose and install another Modrinth or Maven pack version |
| `pastel self-update` | Download, verify, and install the latest Pastel release |
| `pastel run` | Refresh when enabled, then start in the background with crash restarts |
| `pastel run -f` | Run in the foreground attached to this terminal |
| `pastel console` | Follow the live log and send server commands |
| `pastel stop` | Ask Minecraft to save and stop, then escalate if necessary |
| `pastel status` | Show detailed state for this server folder |
| `pastel version` | Print the Pastel version |

`sync` remains an alias for `refresh`; `upgrade` remains an alias for `update`. In scripts, `pastel update -to 1.3.0 -yes` skips the version picker and the confirmation.

After a background server reaches Minecraft's ready state, Pastel restarts it five seconds after an unexpected exit. A normal `pastel stop` or `stop` console command stays stopped. Startup failures also stay stopped so an invalid pack cannot enter a restart loop. Foreground mode stays attached to one Minecraft process and does not restart it.

On Windows, background console commands travel through an owner-restricted named pipe instead of a Unix FIFO. Background `pastel run`, `pastel console`, crash restarts, and `pastel stop` work the same way as on macOS and Linux.

## `server.pastel`

The generated file is intentionally small:

```toml
pack = "modrinth:aristea:0.1.5"
memory = "4G"
sync_on_run = true
auto_restart = true
```

| Key | Meaning |
| --- | --- |
| `pack` | Required pack pin: Modrinth, URL, local path, or Maven coordinate |
| `memory` | Java maximum heap, such as `4G`; defaults to `4G` |
| `sync_on_run` | Refresh before each run; defaults to `true` |
| `auto_restart` | Restart a ready background server after a crash; defaults to `true` |
| `repositories` | Ordered Maven bases for short coordinates; there is no default |
| `java` | Optional Java executable override |
| `server_dir` | Optional server directory relative to `server.pastel` |
| `extra_java_args` | Additional JVM arguments |
| `nogui` | Pass `nogui` to Minecraft; defaults to `true` |

Paths are resolved relative to the directory containing `server.pastel`. When an older file has no `auto_restart`, Pastel adds it with its `true` default, which makes the option visible without changing behavior.

## What refresh changes

For a dedicated server, Pastel:

1. selects `.mrpack` files whose `env.server` is not `unsupported`, including optional files;
2. verifies downloads with the strongest hash published by the pack;
3. applies `overrides/`, followed by `server-overrides/`;
4. installs or aligns the loader from the pack dependencies;
5. removes extra jars from `mods/` and stale managed root launcher jars.

Pastel refuses to let a pack write `world/`, `.pastel/`, or `server.pastel`. It also refuses pack changes while the server is running, including the refresh at the start of `pastel run`. Extra jars under `mods/` are pruned during a normal refresh, so preview a change with `pastel refresh -dry-run` or use `-no-prune` when you deliberately keep local jars. Jars that arrived through `overrides/mods/` or `server-overrides/mods/` are kept.

Set `sync_on_run = false` while debugging local pack changes. `pastel run` will leave pack files alone, while an explicit `pastel refresh` still reconciles them.

## Pack and loader support

Pastel reads [Modrinth `.mrpack`](https://support.modrinth.com/en/articles/8802351-modrinth-modpack-format-mrpack) files.

| Loader | Behavior |
| --- | --- |
| Fabric | Downloads a server launcher matching the pack's Minecraft and Fabric Loader versions |
| NeoForge | Runs the official installer and launches its generated argument file |
| Forge | Runs the official installer and launches its generated argument file |
| Quilt | Uses a pack-provided `quilt-server-launch.jar` |
| Vanilla | Uses a pack-provided `server.jar` |

Pastel derives the minimum Java version from Minecraft: Java 25 for 26.1 and newer, Java 21 for 1.20.5 through 1.21.x, Java 17 for 1.17 through 1.20.4, and Java 8 for older releases.

## Troubleshooting

- If `run` restores a jar you removed, set `sync_on_run = false` before trying again.
- If the server exits during startup, read `logs/latest.log`; Pastel also summarizes common wrong-side mod and memory failures.
- If a deleted server folder left a Java process behind on Linux, use `pastel stop -orphans` or `pastel stop -pid <number>`.
- If a Maven coordinate fails, add its repository to `repositories` or pass `-repo` during install. Pastel never chooses a Maven host on its own.
- For a reproducible bug, open a [GitHub issue](https://github.com/iamkaf/palette/issues).

## For pack authors

Pastel uses standard server-capable `.mrpack` files. You do not need a Pastel-specific manifest: publish the pack on Modrinth, provide a direct `.mrpack` URL, or publish the `.mrpack` in a Maven repository.

- Declare `minecraft` and the loader in `dependencies`.
- Give every `files[]` entry a download URL and hash.
- Mark client-only files with `env.server = "unsupported"`.
- Put shared files in `overrides/` and dedicated-server replacements in `server-overrides/`.
- Do not put worlds in the pack.

## Development

```bash
cargo fmt --all -- --check
cargo clippy -p pastel --locked --all-targets -- -D warnings
cargo test -p pastel --locked
```

`tests/supervise.rs` runs the background supervisor through the real executable with a stand-in server, covering console commands, crash restarts, and stopping during the restart delay.

## Security

Use the repository's [security policy](../SECURITY.md) instead of a public issue for suspected vulnerabilities.

## License

Pastel is licensed under the [Apache License, Version 2.0](../LICENSE).
