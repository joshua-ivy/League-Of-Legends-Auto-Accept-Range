//! Random skin selection — ported from `ui/handlers/randomization_handler.py`.
//!
//! The Python handler's double-click debounce flags were UI-click state, not
//! session state — that guard is the caller's job here (the S4 bridge
//! handler already debounces the inbound click message); these functions
//! key off the observable `random_mode_active` flag instead.

#![allow(dead_code)]

use rand::seq::SliceRandom;

use crate::skins::lcu_ext::ChampionSkinCache;
use crate::skins::state::SkinsShared;

struct RandomOption {
    id: i64,
    name: String,
}

/// Pick a random skin (or, if it has chromas, a random chroma of it) from
/// the cached champion's skin list, excluding the champion's base skin and
/// any entry that is itself a chroma (`is_base_skin` in the Python original).
pub fn select_random_skin(cache: &ChampionSkinCache) -> Option<(String, i64)> {
    let champion_id = cache.champion_id?;
    let base_champion_skin_id = champion_id * 1000;

    let available: Vec<_> = cache
        .skins
        .iter()
        .filter(|s| s.skin_id != base_champion_skin_id && !cache.is_chroma(s.skin_id))
        .collect();
    if available.is_empty() {
        return None;
    }

    let mut rng = rand::thread_rng();
    let selected = available.choose(&mut rng)?;

    let chromas = cache.get_chromas_for_skin(selected.skin_id).unwrap_or(&[]);
    if chromas.is_empty() {
        return Some((selected.skin_name.clone(), selected.skin_id));
    }

    let mut options = Vec::with_capacity(chromas.len() + 1);
    options.push(RandomOption { id: selected.skin_id, name: selected.skin_name.clone() });
    for chroma in chromas {
        let name = if chroma.name.is_empty() { format!("{} Chroma", selected.skin_name) } else { chroma.name.clone() };
        options.push(RandomOption { id: chroma.id, name });
    }
    let picked = options.choose(&mut rng)?;
    Some((picked.name.clone(), picked.id))
}

/// Start randomization: disables historic mode (mirrors the Python
/// `_start_randomization`'s safety disable), then rolls a skin. Returns
/// `false` (and clears random-mode state) if no skin was available.
pub fn start_randomization(shared: &mut SkinsShared, cache: &ChampionSkinCache) -> bool {
    if shared.historic_mode_active {
        shared.historic_mode_active = false;
        shared.historic_selection = None;
    }

    match select_random_skin(cache) {
        Some((name, id)) => {
            shared.random_skin_name = Some(name);
            shared.random_skin_id = Some(id);
            shared.random_mode_active = true;
            true
        }
        None => {
            cancel_randomization(shared);
            false
        }
    }
}

/// `RandomizationHandler.cancel`.
pub fn cancel_randomization(shared: &mut SkinsShared) {
    shared.random_skin_name = None;
    shared.random_skin_id = None;
    shared.random_mode_active = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skins::lcu_ext::{ChromaInfo, SkinInfo};

    fn cache_with_skins(champion_id: i64) -> ChampionSkinCache {
        let mut cache = ChampionSkinCache { champion_id: Some(champion_id), ..Default::default() };
        let base = SkinInfo { skin_id: champion_id * 1000, skin_name: "Base".to_string(), ..Default::default() };
        let alt = SkinInfo { skin_id: champion_id * 1000 + 1, skin_name: "Alt Skin".to_string(), ..Default::default() };
        cache.skin_id_map.insert(base.skin_id, base.clone());
        cache.skin_id_map.insert(alt.skin_id, alt.clone());
        cache.skins.push(base);
        cache.skins.push(alt);
        cache
    }

    #[test]
    fn select_random_skin_never_returns_the_base_skin() {
        let cache = cache_with_skins(99);
        for _ in 0..20 {
            let (_, id) = select_random_skin(&cache).expect("a non-base skin exists");
            assert_ne!(id, 99000);
        }
    }

    #[test]
    fn select_random_skin_excludes_chromas_from_the_top_level_pool() {
        let mut cache = cache_with_skins(99);
        // A third skin so excluding the base AND the chroma-marked entry
        // still leaves something selectable.
        let another = SkinInfo { skin_id: 99002, skin_name: "Another Skin".to_string(), ..Default::default() };
        cache.skin_id_map.insert(another.skin_id, another.clone());
        cache.skins.push(another);
        // Register 99001 as a chroma of some other base — it must never be
        // picked at the top level even though it's in `cache.skins`.
        cache.chroma_id_map.insert(99001, ChromaInfo { id: 99001, ..Default::default() });
        for _ in 0..20 {
            let (_, id) = select_random_skin(&cache).expect("selection still possible via other skins");
            assert_ne!(id, 99001);
        }
    }

    #[test]
    fn start_randomization_disables_historic_mode() {
        let mut shared = SkinsShared::default();
        shared.historic_mode_active = true;
        let cache = cache_with_skins(99);
        assert!(start_randomization(&mut shared, &cache));
        assert!(!shared.historic_mode_active);
        assert!(shared.random_mode_active);
    }

    #[test]
    fn cancel_randomization_clears_all_fields() {
        let mut shared = SkinsShared::default();
        shared.random_skin_name = Some("X".to_string());
        shared.random_skin_id = Some(1);
        shared.random_mode_active = true;
        cancel_randomization(&mut shared);
        assert_eq!(shared.random_skin_name, None);
        assert_eq!(shared.random_skin_id, None);
        assert!(!shared.random_mode_active);
    }
}
