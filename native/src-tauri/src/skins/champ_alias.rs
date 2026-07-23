//! Offline champion_id -> WAD alias lookup from the bundled `champ_alias.json`
//! (`{"523":"Aphelios","62":"MonkeyKing",...}`) — lets custom-mod target
//! detection (`injection::target_detect`) resolve a champion's WAD path
//! segment without the LCU running.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::skins::paths;
use crate::skins::slog::log_warn;

static ALIASES: OnceLock<HashMap<i64, String>> = OnceLock::new();

/// Missing/malformed file yields an empty map rather than panicking —
/// callers just fall back to `None` (same as an unresolvable champion).
fn load() -> HashMap<i64, String> {
    let path = paths::get_asset_path("champ_alias.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            log_warn!("[CHAMP-ALIAS] champ_alias.json not readable at {}: {e}", path.display());
            return HashMap::new();
        }
    };
    match serde_json::from_str::<HashMap<String, String>>(&text) {
        Ok(raw) => raw.into_iter().filter_map(|(k, v)| k.parse::<i64>().ok().map(|id| (id, v))).collect(),
        Err(e) => {
            log_warn!("[CHAMP-ALIAS] champ_alias.json malformed: {e}");
            HashMap::new()
        }
    }
}

/// Champion's WAD alias (e.g. 523 -> "Aphelios", 62 -> "MonkeyKing"), or
/// `None` if unlisted (a champion released after this bundled table).
pub fn champ_alias(champion_id: i64) -> Option<String> {
    ALIASES.get_or_init(load).get(&champion_id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_aliases_resolve() {
        assert_eq!(champ_alias(523), Some("Aphelios".to_string()));
        assert_eq!(champ_alias(62), Some("MonkeyKing".to_string()));
    }

    #[test]
    fn unknown_champion_is_none() {
        assert_eq!(champ_alias(99999999), None);
    }
}
