//! Plain-language summaries of Minecraft startup failures.

use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

/// A readable interpretation of a crash log.
pub struct Summary {
    pub headline: &'static str,
    pub details: Vec<&'static str>,
    pub mods: Vec<String>,
}

pub fn summarize(log: &str) -> Summary {
    if log.is_empty() {
        return Summary {
            headline: "We couldn't read a useful error from the log.",
            details: vec!["Open logs/latest.log if you want the full details."],
            mods: Vec::new(),
        };
    }
    let low = log.to_lowercase();
    let mods = extract_mod_names(log);

    // Client-only or wrong-side mods are the most common cause.
    if low.contains("worldrenderevents")
        || low.contains("fabric/api/client")
        || low.contains("is marked as \"client\"")
        || (low.contains("invalid player data") && low.contains("client"))
    {
        return Summary {
            headline: "A client-only mod is installed on this dedicated server.",
            details: vec![
                "Some mods are for the game you play on your computer, not for a server.",
                "They need to be removed from the server's mods folder.",
            ],
            mods,
        };
    }
    if low.contains("could not execute entrypoint")
        || low.contains("failed to start the minecraft server")
    {
        let mut details = vec!["Usually a bad, outdated, or client-only jar in mods/."];
        if mods.is_empty() {
            details.push("Check logs/latest.log for the mod name if you're unsure.");
        }
        return Summary {
            headline: "A mod failed while Minecraft was starting up.",
            details,
            mods,
        };
    }
    if low.contains("mixin") && (low.contains("error") || low.contains("fail")) {
        return Summary {
            headline: "Mods are conflicting with each other (or with this Minecraft version).",
            details: vec!["Try removing recently added jars from mods/, then run again."],
            mods,
        };
    }
    if low.contains("out of memory") || low.contains("outofmemory") {
        return Summary {
            headline: "The server ran out of memory.",
            details: vec!["Increase memory in server.pastel, for example: memory = \"6G\""],
            mods: Vec::new(),
        };
    }
    if low.contains("unsupported class file")
        || (low.contains("java.lang.class") && low.contains("version"))
    {
        return Summary {
            headline: "This pack needs a different Java version.",
            details: vec![
                "Pastel usually picks Java automatically — try ./pastel run again after a refresh.",
            ],
            mods: Vec::new(),
        };
    }
    Summary {
        headline: "Something in the pack or mods folder stopped the server.",
        details: vec!["This is often a bad jar under mods/, or a pack version mismatch."],
        mods,
    }
}

static PROVIDED_BY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)provided by '([^']+)'").expect("valid pattern"));
static JAR_REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"~\[([a-zA-Z0-9._+-]+\.jar)").expect("valid pattern"));
static MODS_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"mods[/\\]([a-zA-Z0-9._+-]+\.jar)").expect("valid pattern"));

/// Likely mod IDs and jar names from Fabric-style crash text.
fn extract_mod_names(log: &str) -> Vec<String> {
    let candidates = PROVIDED_BY
        .captures_iter(log)
        .take(8)
        .chain(JAR_REFERENCE.captures_iter(log).take(12))
        .chain(MODS_PATH.captures_iter(log).take(8))
        .map(|captures| captures[1].trim().to_owned());
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    for name in candidates {
        let low = name.to_lowercase();
        // Skip the loader and the game themselves.
        if name.is_empty()
            || low.contains("fabric-loader")
            || low.contains("java.base")
            || low.contains("server-intermediary")
            || low == "minecraft"
            || !seen.insert(name.clone())
        {
            continue;
        }
        names.push(name);
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_only_crashes_name_the_mods() {
        let summary = summarize(
            "[main/ERROR]: Failed to start the minecraft server
java.lang.RuntimeException: Could not execute entrypoint stage 'main' due to errors, provided by 'amberdreams' at 'com.iamkaf.amberdreams.fabric.AmberDreamsFabric'!
Caused by: java.lang.NoClassDefFoundError: net/fabricmc/fabric/api/client/rendering/v1/WorldRenderEvents
~[amberdreams-fabric-1.21.1-0.2.0-alpha.1.jar:?]
~[dynamicedge-fabric-1.21.1-1.0.0-alpha.2.jar:?]
",
        );
        assert!(summary.headline.to_lowercase().contains("client"));
        assert_eq!(
            summary.mods,
            [
                "amberdreams",
                "amberdreams-fabric-1.21.1-0.2.0-alpha.1.jar",
                "dynamicedge-fabric-1.21.1-1.0.0-alpha.2.jar"
            ]
        );
    }
}
