# Chalk

Chalk builds and tests Minecraft datapacks that work across Minecraft versions. You write
the pack once for the newest version, put the few files that differ on older versions next
to the files they replace, and Chalk builds one zip that loads on every version you
support. It also runs your pack's tests in a real game on each of those versions.

Chalk is new. Its file layout and commands may change while more packs use it.

## Build and install

Download the archive for your platform from [GitHub Releases](https://github.com/iamkaf/chalk/releases). Each release includes `release-manifest.json` with SHA-256 and SHA-512 hashes and a keyless Sigstore bundle. GitHub also records an artifact attestation for every native archive.

To build from source with stable Rust:

```bash
cargo build --release --locked
./target/release/chalk --help
```

## A pack

```text
my-pack/
├── chalk.toml
├── datapack/
│   ├── pack.png
│   └── data/
└── tests/
    └── portals.test.ts
```

`chalk.toml` names the pack's Minecraft versions and its description:

```toml
description = "Light Nether portals in the End"
minecraft = "1.21.1-26.3"
```

`datapack/` holds everything players get except `pack.mcmeta`, which Chalk writes. The
directory name is the pack's slug, so this one builds `my-pack.zip`.

## Older versions

Write the pack for the newest version in its range. When a file has to be different on
older versions, put the older form next to it and name the versions after an `@`:

```text
data/my_pack/tags/block/frame.json          # 26.3
data/my_pack/tags/block/frame@-26.2.json    # 26.2 and older
```

Versions can be written as `1.21.1-26.2`, `26.2` for one version, `-26.2` for everything up
to it, or `26.2-` for it and newer. Chalk groups these files into overlays and writes a
`pack.mcmeta` that every version in range can read, including the older fields that 1.21.8
and earlier still require. Two files that cover the same versions of the same path are an
error.

## Commands

```bash
chalk check                    # validate the pack and typecheck its tests
chalk test                     # run the tests on every supported version
chalk test --minecraft 26.2    # run them on one version; repeat for more
chalk test --visible           # show the Minecraft window instead of using Xvfb
chalk build                    # build the zip into build/chalk/
```

Chalk writes everything it generates under `build/chalk/`.

## Tests

Tests are TeaKit TypeScript files in `tests/`. `chalk test` runs each one in a Fabric
client connected to a Fabric dedicated server that loads your built zip, with Fabric API,
Sodium, Iris, and C2ME installed on both, so the pack is proven in the game players
actually run. A run also fails when the server logs a warning or error about one of the
pack's namespaces, such as a file that didn't parse on that version.

Chalk tests on these versions:

| Minecraft | Data format |
| --- | --- |
| 26.3 | 121 |
| 26.2 | 107.1 |
| 26.1.2 | 101.1 |
| 1.21.11 | 94.1 |
| 1.21.1 | 48 |

1.21.1 skips C2ME, whose 1.21.1 builds need a newer Java than that game launches with. The
pinned versions live in [`src/environments.toml`](src/environments.toml).

`chalk test` needs Java and [Modstage](https://github.com/iamkaf/modstage) on `PATH`. On
Linux it hides the game with `xvfb-run` unless you pass `--visible`. Before and after each
run, Chalk stops any Minecraft process still running in that version's test instance, so
a server that hangs on shutdown can't lock the next run out of its world.

## License

[Apache 2.0](LICENSE)
