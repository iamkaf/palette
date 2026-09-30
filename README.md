# Palette

Tools for making Minecraft packs.

| Tool | What it does |
| --- | --- |
| [Swatch](swatch) | Builds and publishes lockfile-first modpacks. |
| [Chalk](chalk) | Builds datapacks that work across Minecraft versions and tests them in a real game. |

The tools share one publisher, [`publish`](publish), which prepares a release once and
uploads the same files to GitHub Releases, a Maven repository, Modrinth, and CurseForge.

Each tool is its own command with its own releases. Download them from
[GitHub Releases](https://github.com/iamkaf/palette/releases), where releases are tagged
`<tool>-v<version>`, or build one from source with stable Rust:

```bash
cargo build --release --locked -p chalk
```

## License

[Apache 2.0](LICENSE)
