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
a test for it. `chalk check` runs that test on every version.

## A pack

```text
my-pack/
├── chalk.toml
├── datapack/
│   ├── pack.png
│   └── data/
└── tests/
    ├── lights_a_frame.mcfunction
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
chalk check                    # validate the pack, typecheck its tests, and load it and
                               # run its function tests in every supported version
chalk check --no-game          # skip loading it in Minecraft
chalk dev                      # play the pack on the newest version and reload it on save
chalk dev --minecraft 1.21.1   # play it on another version
chalk test                     # run the TeaKit tests on every supported version
chalk test --minecraft 26.2    # run them on one version; repeat for more
chalk test --visible           # show the Minecraft window instead of using Xvfb
chalk changes                  # list what to look into across the pack's versions
chalk build                    # build the zip into build/chalk/
```

Chalk writes everything it generates under `build/chalk/`.

## Loading in Minecraft

`chalk check` starts a vanilla dedicated server for each supported version with the built
zip in a new world, runs the pack's function tests, and stops it. Whatever the game logged
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

## Function tests

Every `.mcfunction` file directly in `tests/` is a test. Chalk loads them as the `test`
namespace next to your pack and runs each one, so `tests/lights_a_frame.mcfunction` runs
as `function test:lights_a_frame`. Functions in folders under `tests/` are helpers your
tests can call, like `test:helpers/frame`.

A test fails when it returns 0, as `return fail` does, or when it says anything with
`say`. That makes `execute unless ... run say` a one-line assertion whose message Chalk
prints:

```mcfunction
# A 2 by 3 obsidian frame along X lights from fire in its bottom-left cell.
execute in minecraft:the_end run fill 8 100 8 11 104 8 minecraft:obsidian
execute in minecraft:the_end run fill 9 101 8 10 103 8 minecraft:air
execute in minecraft:the_end positioned 9 101 8 run function my_pack:ignite

execute in minecraft:the_end unless block 9 101 8 minecraft:nether_portal run say no portal in the bottom-left cell
```

```text
  26.3     loaded, 1 of 3 tests failed
  FAIL    tests/lights_a_frame.mcfunction
          no portal in the bottom-left cell
```

A server without players has no chunks loaded, so Chalk loads blocks 0 to 31 on X and Z
in the Overworld, the Nether, and the End before the tests run. Build your fixtures there.
Tests share one world, which starts fresh for every check, so give each test its own
spot.

## Porting to a new version

When a new Minecraft version comes out, widen `minecraft` in `chalk.toml` to include it
and run `chalk changes`. It lists every file that uses something vanilla's own data files
use in only some of the versions reading that file: a renamed key, a value that became an
object or a list, an ID that appeared or went away, or a folder that moved.

```text
To look into, from vanilla data across Minecraft 1.21.1-26.3
  datapack/data/my_pack/advancement/light_portal.json (1.21 to 26.3)
    `location/condition`: in vanilla `advancement` files until 26.2
    `conditions/location` as a list: in vanilla `advancement` files until 26.2
```

Keys are named with the key holding them, so `conditions/location` is the `location`
inside `conditions`. Each line is a lead, not a verdict. Vanilla's files show what Mojang
changed in its own data, so a pack using the same thing likely needs a new file for the
newer version, or a variant for the older ones. Something vanilla never uses doesn't show
up, and neither do functions and tags; `chalk check` loads those on every version.

`chalk changes` compares with a table built into Chalk, so it needs no game and gives the
same answer every time.

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

## TeaKit tests

Function tests can't use a player. For that, write TeaKit TypeScript tests as
`tests/*.test.ts`. `chalk test` runs each one in a Fabric
client connected to a Fabric dedicated server that loads your built zip, with Fabric API,
Sodium, Iris, and C2ME installed on both, so the pack is proven in the game players
actually run. A run also fails when the server logs a warning or error about one of the
pack's namespaces, such as a file that didn't parse on that version.

Add `tests/tsconfig.json` containing `{ "extends": "../build/chalk/tsconfig.json" }` so
your editor finds the `@teakit/test` types, which `chalk check` installs there.

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

`chalk check`, `chalk dev`, and `chalk test` need [Modstage](https://github.com/iamkaf/modstage)
0.7.1 or newer on `PATH`. Modstage installs the Java each Minecraft version needs when yours
is too old. On Linux `chalk test` hides the game with `xvfb-run` unless you pass `--visible`. Before and after each
run, Chalk stops any Minecraft process still running in that version's test instance, so
a server that hangs on shutdown can't lock the next run out of its world.

## Publishing

Add a name, a version, and where to publish to `chalk.toml`:

```toml
name = "Nether Portals In The End"
version = "1.0.0"
group = "com.example.datapacks"   # only needed for Maven

[publish]
changelog = "CHANGELOG.md"

[publish.modrinth]
project = "AbCdEfGh"

[publish.maven]
repository = "https://maven.example.com/releases"
```

`chalk publish --dry-run` builds `<slug>-<version>.zip` and shows every upload without making
one. For a real release, `chalk prepare` builds the files into `build/chalk/dist/` from a
clean checkout, `chalk verify` checks they still match, and `chalk publish` uploads those
exact files. The release lists every Minecraft release in the pack's range, not only the
versions Chalk tests, because the zip loads on all of them.

Chalk shares Swatch's publisher, so `[publish.github]`, `[publish.maven]`,
`[publish.modrinth]`, and `[publish.curseforge]` work [the same way](../swatch#prepare-and-publish-a-release).
Publishing reads `GITHUB_TOKEN`, `MAVEN_PUBLISH_USERNAME` and `MAVEN_PUBLISH_PASSWORD`,
`MODRINTH_TOKEN`, and `CURSEFORGE_TOKEN` for the targets you configure.

## Maintaining Chalk

Adding a Minecraft version means adding it to `[releases]` in
[`src/environments.toml`](src/environments.toml) and rebuilding the table `chalk changes`
reads from the dedicated server jars, one per data format, including the new one:

```bash
chalk vanilla-table 1.21.1.jar 1.21.3.jar ... 26.3.jar 26.4.jar > chalk/src/vanilla.json
```

The table lists its jars' versions under `releases`. Server jars come from
`downloads.server.url` in each version's entry in Mojang's version manifest.

## License

[Apache 2.0](../LICENSE)
