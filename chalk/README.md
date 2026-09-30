# Chalk

Chalk develops and tests Minecraft datapacks across Minecraft versions. It checks that a
pack's `pack.mcmeta` loads on every version it claims, runs the pack's tests in a real
game on each of those versions, and zips the pack for players.

## Install

Build from source with stable Rust:

```bash
cargo build --release --locked
./target/release/chalk --help
```

`chalk test` also needs Java, [Modstage](https://github.com/iamkaf/modstage) on `PATH`,
and `xvfb-run` unless you pass `--visible`.

## A pack repository

```text
my-pack/
├── datapack/          # what players install: pack.mcmeta, data/, overlays
└── tests/*.test.ts    # TeaKit tests
```

That's all Chalk needs. The directory name is the pack's slug. Chalk writes everything it
generates under `build/chalk/`.

## Commands

```bash
chalk check                    # validate the pack and typecheck the tests
chalk test                     # run the tests on every version the pack supports
chalk test --minecraft 26.2    # run them on one version; repeat for more
chalk test --visible           # show the Minecraft window instead of using Xvfb
chalk build                    # zip the pack into build/chalk/<slug>.zip
```

## Versions

Chalk tests on these Minecraft versions, newest first:

| Minecraft | Data format |
| --- | --- |
| 26.3 | 121.0 |
| 26.2 | 107.1 |
| 26.1.2 | 101.1 |
| 1.21.11 | 94.1 |
| 1.21.1 | 48 |

A pack supports every version whose format falls inside its `pack.mcmeta` range, and
`chalk test` runs on all of them. Each version runs as a Fabric client connected to a
Fabric dedicated server, both with Fabric API, Sodium, Iris, and C2ME, so the pack is
proven in the game players actually run. 1.21.1 skips C2ME, whose 1.21.1 builds need a
newer Java than that game launches with. The exact versions live in
[`src/environments.toml`](src/environments.toml).

Tests install the zip `chalk build` makes, so they prove what players download. Before and
after each run, Chalk stops any Minecraft process still running in that version's test
instance. A 1.21.1 server can hang while saving on shutdown and would otherwise keep its
world locked.

## One pack for many versions

Minecraft reads different `pack.mcmeta` fields on different versions. A pack that
reaches back to 1.21.8 or older has to declare its range twice: `pack_format` and
`supported_formats` for older games, `min_format` and `max_format` for newer ones.
`chalk check` applies the rules of every version in range and reports which overlays
each version loads:

```json
{
  "pack": {
    "description": "My pack",
    "pack_format": 48,
    "supported_formats": [48, 121],
    "min_format": 48,
    "max_format": 121
  },
  "overlays": {
    "entries": [
      { "directory": "before-26.3", "formats": [48, 107], "min_format": 48, "max_format": 107 }
    ]
  }
}
```

Write the pack for the newest version and put older forms of the files that changed in
an overlay. Older games layer the overlay's files over the base pack.

## License

[Apache 2.0](LICENSE)
