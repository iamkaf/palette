# Chalk

Chalk builds and tests Minecraft datapacks that work across Minecraft versions. You write
the pack once for the newest version, put the few files that differ on older versions next
to the files they replace, and Chalk builds one zip that loads on every version you
support. It also runs your pack's tests in a real game on each of those versions.

Chalk is new. Its file layout and commands may change while more packs use it.

## Build and install

Download the archive for your platform from [Palette's GitHub Releases](https://github.com/iamkaf/palette/releases), where Chalk releases are tagged `chalk-v<version>`. Each release includes `release-manifest.json` with SHA-256 and SHA-512 hashes and a keyless Sigstore bundle. GitHub also records an artifact attestation for every native archive.

To build from source with stable Rust, from the root of this repository:

```bash
cargo build --release --locked -p chalk
./target/release/chalk --help
```

## Start a pack

```bash
chalk init my-pack
cd my-pack
chalk dev
```

`chalk init` creates a pack that supports every version Chalk tests, with one function and
a test for it, and installs the test types so your editor can check `tests/` right away.

## A pack

```text
my-pack/
├── chalk.toml
├── datapack/
│   ├── pack.png
│   └── data/
└── tests/
    ├── portals.test.ts
    └── tsconfig.json
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
chalk init my-pack             # create a pack in a new directory
chalk check                    # validate the pack, typecheck its tests, and load it in
                               # every supported version
chalk check --no-game          # skip loading it in Minecraft
chalk dev                      # play the pack on the newest version and reload it on save
chalk dev --minecraft 1.21.1   # play it on another version
chalk test                     # run the tests on every supported version
chalk test --minecraft 26.2    # run them on one version; repeat for more
chalk test --visible           # show the Minecraft window instead of using Xvfb
chalk build                    # build the zip into build/chalk/
```

Chalk writes everything it generates under `build/chalk/`.

## Loading in Minecraft

`chalk check` starts a vanilla dedicated server for each supported version with the built
zip in its world, waits until the server is up, and stops it. Whatever the game logged
about the pack becomes a problem, pointed at the file that version actually read, and at
the line for commands that don't parse:

```text
Loading the pack in Minecraft
  26.3     loaded
  1.21.1   1 problem
  PACK    datapack/data/my_pack/advancement/light@-26.2.json: Unknown registry key in ResourceKey[minecraft:root / minecraft:trigger_type]: minecraft:no_such_trigger
```

This catches anything the game rejects on a version, like a command that doesn't parse or
an ID that doesn't exist there, without writing a test. Servers start a few at a time;
five versions take about a minute once their files are downloaded.

## Playing while you work

`chalk dev` starts a vanilla server with the pack in its world and prints the address to
join from your own Minecraft. Every time you save a file in `datapack/` or `chalk.toml`,
Chalk rebuilds the pack, reloads it in the running game, and reports what Minecraft said
about it:

```text
Join 127.0.0.1:25565 from Minecraft 26.3. Chalk reloads the pack when you save, and Ctrl+C stops the server.
Reloaded with 1 problem
  PACK    datapack/data/my_pack/function/row.mcfunction:6: Unknown block type 'minecraft:not_a_block'
          fill ~ ~ ~ ~1 ~1 ~1 minecraft:not_a_block
                              ^
Reloaded
```

Players who join become operators, so you can run your functions right away. The server
only accepts connections from your own computer, and the world stays between runs.

## Tests

Tests are TeaKit TypeScript files in `tests/`. `tests/tsconfig.json` extends the config
`chalk check` writes to `build/chalk/`, which is how editors find the `@teakit/test` types. `chalk test` runs each one in a Fabric
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

[Apache 2.0](../LICENSE)
