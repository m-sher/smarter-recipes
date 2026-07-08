//! Prompt construction for batch category labeling.

use super::vocab::{allowlist_for_prompt, MAX_LABELS};
use super::RecipeSnippet;

/// System instruction: closed vocab + classification rules.
pub fn system_instruction() -> String {
    format!(
        "You classify cooking recipes for a meal planner.\n\
         \n\
         Return ONLY a JSON array. Each element is an object:\n\
         {{\"id\": \"<recipe id>\", \"categories\": [\"Label\", ...]}}\n\
         \n\
         Rules:\n\
         - Use ONLY labels from this allowlist (exact spelling):\n\
         {allowlist}\n\
         - Prefer 1–2 labels; never more than {max}.\n\
         - Drinks, coolers, teas, smoothies, cocktails → Beverage and/or Drink.\n\
         - Sauces, dressings, condiments, salsas, spice mixes, how-tos, pure components → those labels only; do NOT add Dinner/Lunch.\n\
         - Full meals → Dinner/Lunch/Breakfast/Main Course/etc. Do not add Component.\n\
         - If unsure, return an empty categories array for that id.\n\
         - Ignore numeric prefixes in titles (e.g. \"356. Watermelon…\").\n\
         - Include every input id exactly once in the output array.\n\
         - No markdown fences, no commentary — JSON array only.",
        allowlist = allowlist_for_prompt(),
        max = MAX_LABELS,
    )
}

/// User message: JSON array of snippets for the batch.
pub fn user_batch_message(items: &[RecipeSnippet]) -> String {
    let payload: Vec<serde_json::Value> = items
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "title": s.title,
                "source": s.source_kind,
                "ingredients": s.ingredients,
            })
        })
        .collect();
    format!(
        "Classify these recipes. Respond with a JSON array of {{id, categories}} only.\n{}",
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "[]".into())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_includes_ids_and_ingredients() {
        let items = vec![RecipeSnippet {
            id: "abc-123".into(),
            title: "Watermelon Cooler".into(),
            ingredients: vec!["1 cup watermelon".into(), "1 cup lemonade".into()],
            source_kind: "epub",
        }];
        let msg = user_batch_message(&items);
        assert!(msg.contains("abc-123"));
        assert!(msg.contains("Watermelon Cooler"));
        assert!(msg.contains("1 cup watermelon"));
        assert!(msg.contains("epub"));
    }

    #[test]
    fn system_mentions_allowlist_and_beverage() {
        let s = system_instruction();
        assert!(s.contains("Beverage"));
        assert!(s.contains("Dinner"));
        assert!(s.contains("empty categories"));
    }
}
