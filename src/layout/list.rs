//! The layouts XKB's descriptions offer, read from their own list of them
//! (`rules/evdev.lst`: a `! layout` section of `code  Name` lines).

use std::path::PathBuf;

/// Where XKB's descriptions are kept: where the environment says, else
/// where distributions put them.
fn roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = std::env::var_os("XKB_CONFIG_ROOT") {
        roots.push(PathBuf::from(root));
    }
    roots.push(PathBuf::from("/usr/share/X11/xkb"));
    roots.push(PathBuf::from("/usr/share/xkeyboard-config-2"));
    roots
}

/// Every layout as (code, name), by name.
pub(super) fn layouts() -> Vec<(String, String)> {
    roots()
        .iter()
        .find_map(|root| std::fs::read_to_string(root.join("rules/evdev.lst")).ok())
        .map(|text| parse(&text))
        .unwrap_or_default()
}

fn parse(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if let Some(section) = line.strip_prefix('!') {
            inside = section.trim() == "layout";
            continue;
        }
        if !inside {
            continue;
        }
        let line = line.trim();
        let Some((code, name)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        // "custom" is a place for a file of the person's own, not a layout.
        if code != "custom" {
            found.push((code.to_string(), name.trim().to_string()));
        }
    }
    found.sort_by(|a, b| a.1.cmp(&b.1));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_layout_section_is_read() {
        let text = "! model\n  pc104  Generic 104-key PC\n\n! layout\n  us              English (US)\n  lt              Lithuanian\n  custom          A user-defined custom Layout\n\n! variant\n  us1  lt: Lithuanian (US)\n";
        assert_eq!(
            parse(text),
            vec![
                ("us".to_string(), "English (US)".to_string()),
                ("lt".to_string(), "Lithuanian".to_string()),
            ]
        );
    }

    #[test]
    fn the_layouts_are_by_name() {
        let text = "! layout\n  se  Swedish\n  al  Albanian\n";
        let names: Vec<String> = parse(text).into_iter().map(|(_, name)| name).collect();
        assert_eq!(names, vec!["Albanian", "Swedish"]);
    }
}
