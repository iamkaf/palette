use crate::Result;
use serde_json::Value;
use std::fmt;
use std::path::Path;

/// The last data pack format before formats gained minor versions (Minecraft 1.21.8).
/// Games up to it only read `pack_format`, `supported_formats`, and overlay `formats`.
const LAST_PRE_MINOR: u32 = 81;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackFormat {
    pub major: u32,
    pub minor: u32,
}

impl PackFormat {
    fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }
}

impl fmt::Display for PackFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.minor == u32::MAX {
            write!(f, "{}.*", self.major)
        } else {
            write!(f, "{}.{}", self.major, self.minor)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatRange {
    pub min: PackFormat,
    pub max: PackFormat,
}

impl FormatRange {
    pub fn contains(&self, format: PackFormat) -> bool {
        self.min <= format && format <= self.max
    }
}

impl fmt::Display for FormatRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} to {}", self.min, self.max)
    }
}

#[derive(Debug)]
pub struct Overlay {
    pub directory: String,
    pub formats: FormatRange,
}

/// The format information in `pack.mcmeta`, checked against the rules of every game
/// version the pack claims.
#[derive(Debug)]
pub struct PackMeta {
    pub formats: FormatRange,
    pub overlays: Vec<Overlay>,
}

impl PackMeta {
    pub fn overlays_for(&self, format: PackFormat) -> Vec<&str> {
        self.overlays
            .iter()
            .filter(|overlay| overlay.formats.contains(format))
            .map(|overlay| overlay.directory.as_str())
            .collect()
    }
}

pub fn read(pack_dir: &Path) -> Result<PackMeta> {
    let path = pack_dir.join("pack.mcmeta");
    let text =
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    let meta = parse(&value).map_err(|error| format!("{}: {error}", path.display()))?;
    for overlay in &meta.overlays {
        if !pack_dir.join(&overlay.directory).is_dir() {
            return Err(format!(
                "{}: overlay directory {} does not exist",
                path.display(),
                overlay.directory
            )
            .into());
        }
    }
    Ok(meta)
}

pub fn parse(value: &Value) -> Result<PackMeta> {
    let pack = value.get("pack").ok_or("missing the pack section")?;
    let declared = Declared::read(pack, "supported_formats")?;
    let formats =
        declared.validate("pack", Some(pack_format(pack)?), false, "supported_formats")?;

    let entries = match value.get("overlays") {
        None => Vec::new(),
        Some(overlays) => overlays
            .get("entries")
            .and_then(Value::as_array)
            .ok_or("overlays needs an entries list")?
            .clone(),
    };
    // Games that predate minor formats require `formats` on every overlay entry they
    // parse, so a pack that loads on those games needs it everywhere.
    let mut declared_overlays = Vec::new();
    for entry in &entries {
        declared_overlays.push((overlay_directory(entry)?, Declared::read(entry, "formats")?));
    }
    let require_old = formats.min.major <= LAST_PRE_MINOR
        || declared_overlays
            .iter()
            .any(|(_, declared)| declared.effective_min_major() <= LAST_PRE_MINOR);
    let mut overlays = Vec::new();
    for (directory, declared) in declared_overlays {
        let context = format!("overlay {directory}");
        let formats = declared.validate(&context, None, require_old, "formats")?;
        overlays.push(Overlay { directory, formats });
    }
    Ok(PackMeta { formats, overlays })
}

fn pack_format(pack: &Value) -> Result<Option<u32>> {
    match pack.get("pack_format") {
        None => Ok(None),
        Some(value) => Ok(Some(as_format_number(value, "pack_format")?)),
    }
}

fn overlay_directory(entry: &Value) -> Result<String> {
    let directory = entry
        .get("directory")
        .and_then(Value::as_str)
        .ok_or("overlay entry is missing its directory")?;
    let valid = !directory.is_empty()
        && directory
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid {
        return Err(format!(
            "overlay directory {directory} may only use letters, digits, -, _, and ."
        )
        .into());
    }
    Ok(directory.to_owned())
}

/// The format fields as written, before the game's consistency rules apply.
struct Declared {
    min: Option<PackFormat>,
    max: Option<PackFormat>,
    /// `supported_formats` in the pack section or `formats` in an overlay.
    old: Option<(u32, u32)>,
}

impl Declared {
    fn read(section: &Value, old_field: &str) -> Result<Self> {
        Ok(Self {
            min: section
                .get("min_format")
                .map(|value| format_field(value, "min_format", 0))
                .transpose()?,
            max: section
                .get("max_format")
                .map(|value| format_field(value, "max_format", u32::MAX))
                .transpose()?,
            old: section
                .get(old_field)
                .map(|value| inclusive_range(value, old_field))
                .transpose()?,
        })
    }

    fn effective_min_major(&self) -> u32 {
        match (self.min, self.old) {
            (Some(min), Some((old_min, _))) => min.major.min(old_min),
            (Some(min), None) => min.major,
            (None, Some((old_min, _))) => old_min,
            (None, None) => u32::MAX,
        }
    }

    /// Mirrors `PackFormat.IntermediaryFormat#validate` from Minecraft 26.3. `pack_format`
    /// is `Some` only for the pack section, where the game also reads that field.
    fn validate(
        &self,
        context: &str,
        pack_format: Option<Option<u32>>,
        require_old: bool,
        old_field: &str,
    ) -> Result<FormatRange> {
        if self.min.is_some() != self.max.is_some() {
            return Err(format!("{context} must declare both min_format and max_format").into());
        }
        if require_old && self.old.is_none() {
            return Err(
                format!("{context} needs {old_field} so games before 1.21.9 can read it").into(),
            );
        }
        if let (Some(min), Some(max)) = (self.min, self.max) {
            if min > max {
                return Err(
                    format!("{context} min_format {min} is greater than max_format {max}").into(),
                );
            }
            if min.major > LAST_PRE_MINOR && !require_old {
                if self.old.is_some() {
                    return Err(format!(
                        "{context} only supports 1.21.9 and newer, so remove {old_field}"
                    )
                    .into());
                }
            } else {
                let (old_min, old_max) = self.old.ok_or_else(|| {
                    format!(
                        "{context} reaches format {} and needs {old_field}: [{}, {}]",
                        min.major, min.major, max.major
                    )
                })?;
                if old_min != min.major {
                    return Err(format!(
                        "{context} {old_field} starts at {old_min} but min_format is {min}"
                    )
                    .into());
                }
                if old_max != max.major && old_max != LAST_PRE_MINOR {
                    return Err(format!(
                        "{context} {old_field} ends at {old_max} but max_format is {max}"
                    )
                    .into());
                }
                if let Some(pack_format) = pack_format {
                    let pack_format = pack_format.ok_or_else(|| {
                        format!(
                            "{context} reaches format {} and needs pack_format",
                            min.major
                        )
                    })?;
                    if pack_format < min.major || pack_format > max.major {
                        return Err(format!(
                            "{context} pack_format {pack_format} is outside {min} to {max}"
                        )
                        .into());
                    }
                }
            }
            return Ok(FormatRange { min, max });
        }
        if let Some((old_min, old_max)) = self.old {
            if old_max > LAST_PRE_MINOR {
                return Err(format!(
                    "{context} reaches format {old_max} and needs min_format and max_format"
                )
                .into());
            }
            return Ok(FormatRange {
                min: PackFormat::new(old_min, 0),
                max: PackFormat::new(old_max, u32::MAX),
            });
        }
        if let Some(Some(format)) = pack_format {
            if format > LAST_PRE_MINOR {
                return Err(format!(
                    "{context} pack_format {format} needs min_format and max_format"
                )
                .into());
            }
            return Ok(FormatRange {
                min: PackFormat::new(format, 0),
                max: PackFormat::new(format, 0),
            });
        }
        Err(format!("{context} declares no format").into())
    }
}

/// `min_format` and `max_format` are `n` or `[major, minor]`. A bare `n` means minor 0 for
/// the minimum and any minor for the maximum.
fn format_field(value: &Value, name: &str, default_minor: u32) -> Result<PackFormat> {
    if let Some(list) = value.as_array() {
        return match list.as_slice() {
            [major] => Ok(PackFormat::new(
                as_format_number(major, name)?,
                default_minor,
            )),
            [major, minor] => Ok(PackFormat::new(
                as_format_number(major, name)?,
                as_format_number(minor, name)?,
            )),
            _ => Err(format!("{name} must be a number or [major, minor]").into()),
        };
    }
    Ok(PackFormat::new(
        as_format_number(value, name)?,
        default_minor,
    ))
}

/// `supported_formats` and overlay `formats` are `n`, `[min, max]`, or
/// `{"min_inclusive": min, "max_inclusive": max}`.
fn inclusive_range(value: &Value, name: &str) -> Result<(u32, u32)> {
    let (min, max) = if let Some(list) = value.as_array() {
        match list.as_slice() {
            [min, max] => (as_format_number(min, name)?, as_format_number(max, name)?),
            _ => return Err(format!("{name} must be [min, max]").into()),
        }
    } else if value.is_object() {
        let min = value
            .get("min_inclusive")
            .ok_or_else(|| format!("{name} is missing min_inclusive"))?;
        let max = value
            .get("max_inclusive")
            .ok_or_else(|| format!("{name} is missing max_inclusive"))?;
        (as_format_number(min, name)?, as_format_number(max, name)?)
    } else {
        let format = as_format_number(value, name)?;
        (format, format)
    };
    if min > max {
        return Err(format!("{name} starts after it ends").into());
    }
    Ok((min, max))
}

fn as_format_number(value: &Value, name: &str) -> Result<u32> {
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| format!("{name} must be a non-negative whole number").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn format(major: u32, minor: u32) -> PackFormat {
        PackFormat::new(major, minor)
    }

    #[test]
    fn a_pack_for_old_and_new_games_carries_both_field_styles() {
        let meta = parse(&json!({
            "pack": {
                "description": "",
                "pack_format": 48,
                "supported_formats": [48, 121],
                "min_format": 48,
                "max_format": 121
            },
            "overlays": {"entries": [
                {"directory": "before-26.3", "formats": [48, 107], "min_format": 48, "max_format": 107}
            ]}
        }))
        .expect("valid multi-version pack");

        assert!(meta.formats.contains(format(48, 0)));
        assert!(meta.formats.contains(format(121, 0)));
        assert!(!meta.formats.contains(format(122, 0)));
        assert_eq!(meta.overlays_for(format(107, 1)), vec!["before-26.3"]);
        assert!(meta.overlays_for(format(121, 0)).is_empty());
    }

    #[test]
    fn an_old_game_needs_pack_format_and_supported_formats() {
        let without_supported = parse(&json!({
            "pack": {"description": "", "pack_format": 48, "min_format": 48, "max_format": 121}
        }));
        assert!(without_supported.is_err());

        let without_pack_format = parse(&json!({
            "pack": {"description": "", "supported_formats": [48, 121], "min_format": 48, "max_format": 121}
        }));
        assert!(without_pack_format.is_err());
    }

    #[test]
    fn a_new_only_pack_rejects_supported_formats() {
        let meta =
            parse(&json!({"pack": {"description": "", "min_format": 94, "max_format": 121}}))
                .expect("new-only pack");
        assert_eq!(
            meta.formats,
            FormatRange {
                min: format(94, 0),
                max: format(121, u32::MAX)
            }
        );

        let with_supported = parse(&json!({
            "pack": {"description": "", "supported_formats": [94, 121], "min_format": 94, "max_format": 121}
        }));
        assert!(with_supported.is_err());
    }

    #[test]
    fn every_overlay_needs_formats_when_old_games_read_the_pack() {
        // The overlay itself only targets new games, but 1.21.1 still parses it.
        let result = parse(&json!({
            "pack": {"description": "", "pack_format": 48, "supported_formats": [48, 121], "min_format": 48, "max_format": 121},
            "overlays": {"entries": [{"directory": "new", "min_format": 121, "max_format": 121}]}
        }));
        assert!(result.is_err());
    }
}
