//! Time-of-day slot steering for meal plans.
//!
//! When enabled, each in-day meal index maps to an optional required
//! [`TodKind`]. Recipes declare zero or more kinds via `meta.tags` and
//! `meta.category` tokens. Enforcement is soft: mismatches are counted and
//! minimized after nutrition violations, before net ingredient union.

use crate::domain::{normalize_title_key, Recipe};

/// Breakfast, lunch, or dinner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TodKind {
    Breakfast,
    Lunch,
    Dinner,
}

impl TodKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TodKind::Breakfast => "breakfast",
            TodKind::Lunch => "lunch",
            TodKind::Dinner => "dinner",
        }
    }
}

impl std::fmt::Display for TodKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Bitset of [`TodKind`] labels on a recipe (`0` = unlabeled).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TodLabels(u8);

const BIT_BREAKFAST: u8 = 1;
const BIT_LUNCH: u8 = 2;
const BIT_DINNER: u8 = 4;

impl TodLabels {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn insert(&mut self, kind: TodKind) {
        self.0 |= kind_bit(kind);
    }

    pub fn contains(self, kind: TodKind) -> bool {
        self.0 & kind_bit(kind) != 0
    }

    /// Human-readable label list for CLI summaries (`"none"` if empty).
    pub fn describe(self) -> String {
        if self.is_empty() {
            return "none".into();
        }
        let mut parts = Vec::new();
        for kind in [TodKind::Breakfast, TodKind::Lunch, TodKind::Dinner] {
            if self.contains(kind) {
                parts.push(kind.as_str());
            }
        }
        parts.join("+")
    }
}

fn kind_bit(kind: TodKind) -> u8 {
    match kind {
        TodKind::Breakfast => BIT_BREAKFAST,
        TodKind::Lunch => BIT_LUNCH,
        TodKind::Dinner => BIT_DINNER,
    }
}

fn apply_token(labels: &mut TodLabels, token: &str) {
    let key = normalize_title_key(token);
    if key.is_empty() {
        return;
    }
    match key.as_str() {
        "breakfast" => labels.insert(TodKind::Breakfast),
        "lunch" => labels.insert(TodKind::Lunch),
        "dinner" => labels.insert(TodKind::Dinner),
        "brunch" => {
            labels.insert(TodKind::Breakfast);
            labels.insert(TodKind::Lunch);
        }
        "supper" => labels.insert(TodKind::Dinner),
        _ => {}
    }
}

/// Collect TOD labels from `meta.tags` and comma-split `meta.category`.
pub fn recipe_tod_labels(recipe: &Recipe) -> TodLabels {
    let mut labels = TodLabels::empty();
    for tag in &recipe.meta.tags {
        apply_token(&mut labels, tag);
    }
    if let Some(cat) = &recipe.meta.category {
        for part in cat.split(',') {
            apply_token(&mut labels, part);
        }
    }
    labels
}

/// Required TOD for in-day meal index `meal` when there are `meals_per_day`
/// slots (`None` = any recipe accepted).
pub fn slot_requirement(meals_per_day: u32, meal: u32) -> Option<TodKind> {
    let m = meals_per_day.max(1);
    if meal >= m {
        return None;
    }
    match m {
        1 => None,
        2 => match meal {
            0 => Some(TodKind::Breakfast),
            _ => Some(TodKind::Dinner),
        },
        _ => {
            if meal == 0 {
                Some(TodKind::Breakfast)
            } else if meal + 1 == m {
                Some(TodKind::Dinner)
            } else if meal == (m - 1) / 2 {
                Some(TodKind::Lunch)
            } else {
                None
            }
        }
    }
}

/// True when `labels` satisfy `required` (`None` accepts every recipe).
pub fn tod_fits(labels: TodLabels, required: Option<TodKind>) -> bool {
    match required {
        None => true,
        Some(kind) => labels.contains(kind),
    }
}

/// Count slots whose required kind is missing from the placed recipe's labels.
pub fn count_tod_misses(labels: &[TodLabels], indices: &[usize], meals_per_day: u32) -> usize {
    let mpd = meals_per_day.max(1);
    indices
        .iter()
        .enumerate()
        .filter(|(slot, &ri)| {
            let meal = (*slot as u32) % mpd;
            let req = slot_requirement(mpd, meal);
            !tod_fits(labels[ri], req)
        })
        .count()
}

/// One mismatched slot for CLI reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodMismatch {
    pub day: u32,
    pub meal: u32,
    pub expected: TodKind,
    pub recipe_title: String,
    pub labels: TodLabels,
}

/// List TOD mismatches for a schedule in plan order.
pub fn tod_mismatches(
    pool: &[&Recipe],
    labels: &[TodLabels],
    indices: &[usize],
    meals_per_day: u32,
) -> Vec<TodMismatch> {
    let mpd = meals_per_day.max(1);
    let mut out = Vec::new();
    for (slot, &ri) in indices.iter().enumerate() {
        let day = slot as u32 / mpd;
        let meal = slot as u32 % mpd;
        if let Some(expected) = slot_requirement(mpd, meal) {
            if !labels[ri].contains(expected) {
                out.push(TodMismatch {
                    day,
                    meal,
                    expected,
                    recipe_title: pool[ri].title.clone(),
                    labels: labels[ri],
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Recipe;

    fn rec_with(title: &str, tags: &[&str], category: Option<&str>) -> Recipe {
        let mut r = Recipe::new(title);
        r.meta.tags = tags.iter().map(|t| (*t).to_string()).collect();
        r.meta.category = category.map(str::to_string);
        r
    }

    #[test]
    fn slot_template_one_to_six() {
        assert_eq!(slot_requirement(1, 0), None);

        assert_eq!(slot_requirement(2, 0), Some(TodKind::Breakfast));
        assert_eq!(slot_requirement(2, 1), Some(TodKind::Dinner));

        assert_eq!(slot_requirement(3, 0), Some(TodKind::Breakfast));
        assert_eq!(slot_requirement(3, 1), Some(TodKind::Lunch));
        assert_eq!(slot_requirement(3, 2), Some(TodKind::Dinner));

        // M=4 → [B, L, Any, D]
        assert_eq!(slot_requirement(4, 0), Some(TodKind::Breakfast));
        assert_eq!(slot_requirement(4, 1), Some(TodKind::Lunch));
        assert_eq!(slot_requirement(4, 2), None);
        assert_eq!(slot_requirement(4, 3), Some(TodKind::Dinner));

        // M=5 → [B, Any, L, Any, D]
        assert_eq!(slot_requirement(5, 0), Some(TodKind::Breakfast));
        assert_eq!(slot_requirement(5, 1), None);
        assert_eq!(slot_requirement(5, 2), Some(TodKind::Lunch));
        assert_eq!(slot_requirement(5, 3), None);
        assert_eq!(slot_requirement(5, 4), Some(TodKind::Dinner));

        // M=6 → [B, Any, L, Any, Any, D]
        assert_eq!(slot_requirement(6, 0), Some(TodKind::Breakfast));
        assert_eq!(slot_requirement(6, 1), None);
        assert_eq!(slot_requirement(6, 2), Some(TodKind::Lunch));
        assert_eq!(slot_requirement(6, 3), None);
        assert_eq!(slot_requirement(6, 4), None);
        assert_eq!(slot_requirement(6, 5), Some(TodKind::Dinner));
    }

    #[test]
    fn labels_from_tags_and_category() {
        let r = rec_with("Pancakes", &["Breakfast"], None);
        let l = recipe_tod_labels(&r);
        assert!(l.contains(TodKind::Breakfast));
        assert!(!l.contains(TodKind::Dinner));

        let r = rec_with("Roast", &[], Some("Main Course, Dinner"));
        let l = recipe_tod_labels(&r);
        assert!(l.contains(TodKind::Dinner));
        assert!(!l.contains(TodKind::Lunch));

        let r = rec_with("Both", &["dinner"], Some("Breakfast"));
        let l = recipe_tod_labels(&r);
        assert!(l.contains(TodKind::Breakfast));
        assert!(l.contains(TodKind::Dinner));
    }

    #[test]
    fn synonyms_brunch_and_supper() {
        let r = rec_with("Brunch Bowl", &["brunch"], None);
        let l = recipe_tod_labels(&r);
        assert!(l.contains(TodKind::Breakfast));
        assert!(l.contains(TodKind::Lunch));
        assert!(!l.contains(TodKind::Dinner));

        let r = rec_with("Supper Stew", &[], Some("Supper"));
        let l = recipe_tod_labels(&r);
        assert!(l.contains(TodKind::Dinner));
    }

    #[test]
    fn unlabeled_fits_only_any() {
        let r = rec_with("Mystery", &["vegetarian"], Some("Main Course"));
        let l = recipe_tod_labels(&r);
        assert!(l.is_empty());
        assert!(tod_fits(l, None));
        assert!(!tod_fits(l, Some(TodKind::Breakfast)));
        assert!(!tod_fits(l, Some(TodKind::Lunch)));
        assert!(!tod_fits(l, Some(TodKind::Dinner)));
    }

    #[test]
    fn count_misses_row_major() {
        let pool = [
            rec_with("B", &["breakfast"], None),
            rec_with("D", &["dinner"], None),
            rec_with("X", &[], None),
        ];
        let labels: Vec<_> = pool.iter().map(recipe_tod_labels).collect();
        let refs: Vec<_> = pool.iter().collect();
        // 2 meals/day: slots B, D — perfect.
        assert_eq!(count_tod_misses(&labels, &[0, 1], 2), 0);
        // dinner in breakfast slot.
        assert_eq!(count_tod_misses(&labels, &[1, 0], 2), 2);
        // unlabeled in breakfast.
        assert_eq!(count_tod_misses(&labels, &[2], 2), 1);
        let miss = tod_mismatches(&refs, &labels, &[2, 1], 2);
        assert_eq!(miss.len(), 1);
        assert_eq!(miss[0].day, 0);
        assert_eq!(miss[0].meal, 0);
        assert_eq!(miss[0].expected, TodKind::Breakfast);
    }
}
