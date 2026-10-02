//! A few keys from `server.properties`.

use std::path::Path;

/// The server identity people recognize, from `server.properties`.
#[derive(Debug, Default)]
pub struct Info {
    pub motd: String,
    pub port: String,
    pub whitelist: bool,
}

/// Reads `server.properties`, or `None` before the server's first run.
pub fn read(root: &Path) -> Option<Info> {
    let text = std::fs::read_to_string(root.join("server.properties")).ok()?;
    let mut info = Info {
        motd: "A Minecraft Server".to_owned(),
        port: "25565".to_owned(),
        whitelist: false,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "motd" if !value.is_empty() => info.motd = unescape_motd(value),
            "server-port" if !value.is_empty() => info.port = value.to_owned(),
            "white-list" | "whitelist" => info.whitelist = is_true(value),
            // Some setups only set enforce-whitelist; treat either as whitelist mode.
            "enforce-whitelist" if is_true(value) => info.whitelist = true,
            _ => {}
        }
    }
    Some(info)
}

fn is_true(value: &str) -> bool {
    matches!(value.to_lowercase().as_str(), "true" | "1" | "yes" | "on")
}

/// Handles common escapes and strips `§` color codes for terminal display.
fn unescape_motd(value: &str) -> String {
    let value = value.replace("\\n", " ").replace("\\u00a7", "§");
    let mut out = String::new();
    let mut skip = false;
    for c in value.chars() {
        if skip {
            skip = false;
        } else if c == '§' {
            skip = true;
        } else {
            out.push(c);
        }
    }
    let out = out.trim();
    if out.is_empty() {
        "A Minecraft Server".to_owned()
    } else {
        out.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motd_color_codes_are_stripped_without_breaking_utf8() {
        assert_eq!(unescape_motd("§aBienvenue §lça va"), "Bienvenue ça va");
    }
}
