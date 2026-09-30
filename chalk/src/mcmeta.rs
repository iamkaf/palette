//! Writes `pack.mcmeta` so every game in a pack's range can read it.

use crate::Result;
use crate::pack::{self, LAST_PRE_MINOR, PackFormat};
use crate::source::Pack;
use serde_json::{Value, json};

pub fn generate(pack: &Pack) -> Result<Value> {
    // Games up to 1.21.8 only read the old fields, so a pack reaching them declares its
    // range both ways, and so does every overlay.
    let old = pack.formats.min.major <= LAST_PRE_MINOR;
    let mut section = json!({ "description": pack.description });
    if old {
        section["pack_format"] = json!(pack.formats.min.major);
        section["supported_formats"] = json!([pack.formats.min.major, pack.formats.max.major]);
    }
    section["min_format"] = format(pack.formats.min, 0);
    section["max_format"] = format(pack.formats.max, u32::MAX);

    let mut mcmeta = json!({ "pack": section });
    if !pack.overlays.is_empty() {
        let entries: Vec<Value> = pack
            .overlays
            .iter()
            .map(|overlay| {
                let mut entry = json!({ "directory": overlay.directory });
                if old {
                    entry["formats"] =
                        json!([overlay.formats.min.major, overlay.formats.max.major]);
                }
                entry["min_format"] = format(overlay.formats.min, 0);
                entry["max_format"] = format(overlay.formats.max, u32::MAX);
                entry
            })
            .collect();
        mcmeta["overlays"] = json!({ "entries": entries });
    }

    // Holds Chalk to the rules each game version applies when it reads the file.
    pack::parse(&mcmeta)
        .map_err(|error| format!("Chalk generated an invalid pack.mcmeta: {error}"))?;
    Ok(mcmeta)
}

/// A bare number means minor 0 for a minimum and any minor for a maximum.
fn format(format: PackFormat, bare_minor: u32) -> Value {
    if format.minor == bare_minor {
        json!(format.major)
    } else {
        json!([format.major, format.minor])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::FormatRange;
    use crate::source::Overlay;
    use crate::versions::VersionRange;

    fn range(min: u32, max: u32) -> FormatRange {
        FormatRange {
            min: PackFormat {
                major: min,
                minor: 0,
            },
            max: PackFormat {
                major: max,
                minor: u32::MAX,
            },
        }
    }

    fn pack(formats: FormatRange, overlays: Vec<Overlay>) -> Pack {
        Pack {
            description: "Test".into(),
            minecraft: VersionRange::parse("1.21.1-26.3").expect("range"),
            formats,
            files: Vec::new(),
            overlays,
        }
    }

    #[test]
    fn a_pack_reaching_old_games_declares_its_range_both_ways() {
        let overlay = Overlay {
            directory: "1.21-26.2".into(),
            formats: range(48, 107),
            files: Vec::new(),
        };
        let mcmeta = generate(&pack(range(48, 121), vec![overlay])).expect("valid");

        assert_eq!(mcmeta["pack"]["pack_format"], 48);
        assert_eq!(mcmeta["pack"]["supported_formats"], json!([48, 121]));
        assert_eq!(mcmeta["pack"]["max_format"], 121);
        assert_eq!(
            mcmeta["overlays"]["entries"][0]["formats"],
            json!([48, 107])
        );
    }

    #[test]
    fn a_pack_for_new_games_only_uses_the_new_fields() {
        let mcmeta = generate(&pack(range(94, 121), Vec::new())).expect("valid");
        assert!(mcmeta["pack"].get("pack_format").is_none());
        assert!(mcmeta["pack"].get("supported_formats").is_none());
        assert!(mcmeta.get("overlays").is_none());
    }
}
