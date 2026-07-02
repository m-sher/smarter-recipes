//! CLI command definitions and handlers.

use crate::domain::{IngredientKey, MealPlan, Recipe, ShoppingList, UnitKind};
use crate::ingest::{
    ingest_from, normalize_url, read_manual_recipe, recipe_source_url, scrape_new_recipes,
    HttpFetcher, ScrapeEvent,
};
use crate::normalize::normalize_line;
use crate::planning::{plan_meals, PlanOptions};
use crate::pricing::{
    enrich_catalog_from_source, FixtureStoreSource, OpenFoodFactsSource, PackageCatalog,
};
use crate::shopping::{
    pantry_keys_for_planning, restock_plan_from_shop, shopping_list_for_plan,
    trip_breakdown_for_plan,
};
use crate::storage::Store;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "smarter-recipes",
    version,
    about = "Ingest recipes, plan meals to minimize distinct ingredients, optimize shopping lists"
)]
pub struct Cli {
    /// Path to SQLite database (default: platform data dir)
    #[arg(long, global = true, env = "SMARTER_RECIPES_DB")]
    pub db: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Ingest a recipe from a source (file, url, image, auto, or manual)
    Import {
        /// Source kind: file | url | image | auto | manual
        source: String,
        /// Path, URL, or image path. Omit for `manual` to enter interactively.
        input: Option<String>,
        /// Print recipe as JSON instead of saving
        #[arg(long)]
        dry_run: bool,
    },
    /// Crawl a parent/index URL for recipe pages and import new ones
    Scrape {
        /// Index/parent URL, e.g. https://example.com/recipes
        url: String,
        /// Max number of new (not-yet-known) pages to fetch this run
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Number of pages to fetch concurrently
        #[arg(long, default_value_t = 8)]
        jobs: usize,
        /// How deep to follow descendant links under the seed path (1 = seed's links only)
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// Re-attempt URLs previously recorded as failures
        #[arg(long)]
        retry_failed: bool,
        /// Discover and report without saving
        #[arg(long)]
        dry_run: bool,
    },
    /// List stored recipes
    List {
        /// Filter titles (case-insensitive substring)
        #[arg(long, short)]
        filter: Option<String>,
        /// Show full IDs
        #[arg(long)]
        full_id: bool,
    },
    /// Show a recipe by id (prefix match allowed if unique)
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete a recipe by id
    Delete { id: String },
    /// Generate a meal plan minimizing distinct ingredients (no recipe repeats)
    Plan {
        /// Number of days
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Meals per day
        #[arg(long, default_value_t = 1)]
        per_day: u32,
        /// Restrict pool to these recipe ids (comma-separated); default: all
        #[arg(long)]
        pool: Option<String>,
        /// Print plan as JSON
        #[arg(long)]
        json: bool,
        /// Do not save the plan
        #[arg(long)]
        dry_run: bool,
    },
    /// List saved meal plans
    Plans,
    /// Show a saved plan
    ShowPlan {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Produce an optimized shopping list for a plan
    Shop {
        /// Plan id (prefix match allowed if unique)
        plan: String,
        /// Optional JSON catalog overlay for package sizes/prices
        #[arg(long)]
        catalog: Option<PathBuf>,
        /// Show per-trip / ordering benefit breakdown (new ingredients by meal/day)
        #[arg(long)]
        trips: bool,
        /// Fetch package sizes from a store source (openfoodfacts | fixture)
        #[arg(long)]
        fetch_prices: Option<String>,
        /// Fixture JSON path when --fetch-prices=fixture (default: fixtures/store_catalog.json)
        #[arg(long)]
        store_fixture: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Export a recipe to JSON file
    Export {
        id: String,
        #[arg(long, short)]
        output: PathBuf,
    },
    /// Print database path and recipe/plan counts
    Status,
    /// Re-parse stored ingredient lines with the current parser (after normalize
    /// improvements). Pass a recipe id, or `--all` to reparse every recipe.
    Reparse {
        /// Recipe id (prefix match). Omit when using --all.
        id: Option<String>,
        /// Reparse every stored recipe
        #[arg(long)]
        all: bool,
    },
    /// Track on-hand ingredients (pantry stock)
    Pantry {
        #[command(subcommand)]
        action: PantryCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum PantryCmd {
    /// List on-hand stock (canonical quantities)
    List,
    /// Add quantity from a free-text ingredient line (e.g. "2 cups milk")
    Add {
        /// Free-text ingredient line with quantity and unit
        line: String,
    },
    /// Set absolute quantity from a free-text ingredient line
    Set {
        /// Free-text ingredient line with quantity and unit
        line: String,
    },
    /// Remove an ingredient from the pantry by name
    Remove {
        /// Ingredient name (normalized match)
        name: String,
        /// Disambiguate when the same name exists under multiple unit kinds
        #[arg(long, value_parser = parse_kind_arg)]
        kind: Option<UnitKind>,
    },
    /// Clear the entire pantry
    Clear {
        /// Required confirmation flag
        #[arg(long)]
        yes: bool,
    },
    /// Complete a shopping trip for a plan: add purchased packages, then deduct
    /// cooked recipe amounts (net = packaging leftovers). Once per plan.
    Restock {
        /// Plan id (prefix match allowed if unique)
        plan: String,
        /// Optional JSON catalog overlay (same as `shop`)
        #[arg(long)]
        catalog: Option<PathBuf>,
    },
}

fn parse_kind_arg(s: &str) -> std::result::Result<UnitKind, String> {
    match s.to_lowercase().as_str() {
        "mass" | "g" | "weight" => Ok(UnitKind::Mass),
        "volume" | "vol" | "ml" => Ok(UnitKind::Volume),
        "count" | "ea" | "each" => Ok(UnitKind::Count),
        "other" => Ok(UnitKind::Other),
        other => Err(format!(
            "unknown kind '{other}' (use mass, volume, count, or other)"
        )),
    }
}

pub fn run(cli: Cli) -> Result<()> {
    let db_path = cli.db.unwrap_or_else(Store::default_path);
    let store = Store::open(&db_path)?;

    match cli.command {
        Commands::Import {
            source,
            input,
            dry_run,
        } => {
            let interactive = matches!(source.to_lowercase().as_str(), "manual" | "interactive")
                && input.as_deref().is_none_or(|s| s.is_empty() || s == "-");
            let recipe = if interactive {
                read_manual_recipe(&mut std::io::stdin().lock(), &mut std::io::stderr())?
            } else {
                let input = input
                    .ok_or_else(|| anyhow::anyhow!("input is required for source '{source}'"))?;
                ingest_from(&source, &input)?
            };
            print_recipe_summary(&recipe);
            if dry_run {
                println!("{}", serde_json::to_string_pretty(&recipe)?);
            } else if store.is_duplicate(&recipe.title, recipe_source_url(&recipe).as_deref())? {
                println!(
                    "Skipped: already have a recipe with this URL or title ({})",
                    recipe.title
                );
            } else {
                store.save_recipe(&recipe)?;
                println!("Saved recipe {} to {}", recipe.id, store.path().display());
            }
        }
        Commands::Scrape {
            url,
            limit,
            jobs,
            depth,
            retry_failed,
            dry_run,
        } => {
            // Skip URLs already imported, plus previously-failed URLs unless retrying.
            let mut skip: HashSet<String> = store
                .list_recipes(None)?
                .iter()
                .filter_map(recipe_source_url)
                .map(|u| normalize_url(&u))
                .collect();
            if !retry_failed {
                skip.extend(store.failed_scrape_urls()?);
            }

            eprintln!("Scanning {url} (depth {depth}) …");
            let fetcher = HttpFetcher::default();
            let outcome = scrape_new_recipes(
                &fetcher,
                &url,
                limit,
                &skip,
                jobs,
                depth,
                &|event| {
                    match event {
                    ScrapeEvent::Planned {
                        candidates,
                        skipped,
                        to_fetch,
                    } => eprintln!(
                        "Found {candidates} same-host link(s); {skipped} already known; queue {to_fetch} …"
                    ),
                    ScrapeEvent::Imported { url, title } => eprintln!("  ✓ {title}  ({url})"),
                    ScrapeEvent::NotRecipe { url, reason } => {
                        eprintln!("  · nav {url}  ({reason})")
                    }
                    ScrapeEvent::Failed { url, reason } => {
                        eprintln!("  ✗ fetch {url}  ({reason})")
                    }
                }
                },
            )?;

            let mut skipped_dup = 0usize;
            let mut saved = 0usize;
            for recipe in &outcome.recipes {
                let source = recipe_source_url(recipe);
                if store.is_duplicate(&recipe.title, source.as_deref())? {
                    skipped_dup += 1;
                    continue;
                }
                if !dry_run {
                    store.save_recipe(recipe)?;
                    // A URL that now succeeds should no longer be remembered as failed.
                    if let Some(u) = source {
                        store.clear_scrape_failure(&normalize_url(&u))?;
                    }
                }
                saved += 1;
            }
            // Persist only hard fetch failures — nav/category pages stay re-crawlable.
            if !dry_run {
                for (link, reason) in &outcome.failed {
                    store.record_scrape_failure(&normalize_url(link), reason)?;
                }
            }

            if dry_run {
                println!(
                    "(dry run) {} new recipe(s), {} nav (not recipe), {} fetch failed, {} skipped known URL, {} would skip URL/title dup — nothing saved",
                    saved,
                    outcome.not_recipe.len(),
                    outcome.failed.len(),
                    outcome.skipped_existing,
                    skipped_dup
                );
            } else {
                println!(
                    "Imported {} new recipe(s) to {} ({} nav, {} fetch failed, {} skipped known URL, {} skipped URL/title dup)",
                    saved,
                    store.path().display(),
                    outcome.not_recipe.len(),
                    outcome.failed.len(),
                    outcome.skipped_existing,
                    skipped_dup
                );
            }
        }
        Commands::List { filter, full_id } => {
            let recipes = store.list_recipes(filter.as_deref())?;
            if recipes.is_empty() {
                println!("No recipes found.");
            }
            for r in recipes {
                let id = if full_id {
                    r.id.as_str().to_string()
                } else {
                    short_id(r.id.as_str())
                };
                let n = r.ingredients.len();
                println!("{id}  {}  ({n} ingredients)", r.title);
            }
        }
        Commands::Show { id, json } => {
            let recipe = resolve_recipe(&store, &id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&recipe)?);
            } else {
                print_recipe_detail(&recipe);
            }
        }
        Commands::Delete { id } => {
            let recipe = resolve_recipe(&store, &id)?;
            store.delete_recipe(recipe.id.as_str())?;
            println!("Deleted {}", recipe.id);
        }
        Commands::Plan {
            days,
            per_day,
            pool,
            json,
            dry_run,
        } => {
            let recipes = load_pool(&store, pool.as_deref())?;
            if recipes.is_empty() {
                bail!("recipe pool is empty; import recipes first");
            }
            let pantry_items = store.list_pantry()?;
            let pantry = pantry_keys_for_planning(&pantry_items);
            let opts = PlanOptions {
                days,
                meals_per_day: per_day,
                pantry,
            };
            let plan = plan_meals(&recipes, &opts);
            if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                print_plan(&plan);
            }
            if !dry_run {
                store.save_plan(&plan)?;
                println!("\nSaved plan {} to {}", plan.id, store.path().display());
            }
        }
        Commands::Plans => {
            let plans = store.list_plans()?;
            if plans.is_empty() {
                println!("No plans found.");
            }
            for p in plans {
                println!(
                    "{}  {} day(s) × {} meal(s)/day  ({} meals)",
                    short_id(&p.id),
                    p.days,
                    p.meals_per_day,
                    p.meals.len()
                );
            }
        }
        Commands::ShowPlan { id, json } => {
            let plan = resolve_plan(&store, &id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                print_plan(&plan);
            }
        }
        Commands::Shop {
            plan,
            catalog,
            trips,
            fetch_prices,
            store_fixture,
            json,
        } => {
            let plan = resolve_plan(&store, &plan)?;
            let mut cat = PackageCatalog::with_defaults();
            if let Some(path) = catalog {
                cat.load_json_overlay(&path)
                    .with_context(|| format!("loading catalog {}", path.display()))?;
            }
            if let Some(ref src_name) = fetch_prices {
                let names: Vec<String> = {
                    let ids: Vec<_> = plan.meals.iter().map(|m| m.recipe_id.clone()).collect();
                    let req = store.aggregate_ingredients(&ids)?;
                    req.into_iter()
                        .map(|(k, _)| k.name)
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .collect()
                };
                let notes = match src_name.to_lowercase().as_str() {
                    "openfoodfacts" | "off" => {
                        let src = OpenFoodFactsSource::default();
                        enrich_catalog_from_source(&mut cat, &src, &names)
                    }
                    "fixture" => {
                        let path = store_fixture
                            .unwrap_or_else(|| PathBuf::from("fixtures/store_catalog.json"));
                        let src = FixtureStoreSource::new(path);
                        enrich_catalog_from_source(&mut cat, &src, &names)
                    }
                    other => bail!(
                        "unknown --fetch-prices source '{other}' (use openfoodfacts or fixture)"
                    ),
                };
                for n in &notes {
                    eprintln!("{n}");
                }
            }
            let list = shopping_list_for_plan(&store, &plan, &cat)?;
            let trip_info = if trips {
                Some(trip_breakdown_for_plan(&store, &plan)?)
            } else {
                None
            };
            if json {
                if let Some(t) = trip_info {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "shopping_list": list,
                            "trips": t,
                        }))?
                    );
                } else {
                    println!("{}", serde_json::to_string_pretty(&list)?);
                }
            } else {
                print_shopping_list(&list);
                if let Some(t) = trip_info {
                    println!(
                        "
--- Trip / ordering analysis ---
"
                    );
                    for s in &t.steps {
                        println!(
                            "Day {} meal {}: {} — {} new ingredient(s) (cumulative {})",
                            s.day + 1,
                            s.meal + 1,
                            s.recipe_title,
                            s.new_count,
                            s.cumulative_unique
                        );
                        for k in &s.new_ingredient_keys {
                            println!("    + {k}");
                        }
                    }
                    println!(
                        "
{}",
                        t.summary
                    );
                }
            }
        }
        Commands::Export { id, output } => {
            let recipe = resolve_recipe(&store, &id)?;
            let text = serde_json::to_string_pretty(&recipe)?;
            std::fs::write(&output, text)
                .with_context(|| format!("writing {}", output.display()))?;
            println!("Wrote {}", output.display());
        }
        Commands::Status => {
            let recipes = store.list_recipes(None)?;
            let plans = store.list_plans()?;
            let pantry = store.list_pantry()?;
            println!("Database: {}", store.path().display());
            println!("Recipes:  {}", recipes.len());
            println!("Plans:    {}", plans.len());
            println!("Pantry:   {} item(s)", pantry.len());
        }
        Commands::Pantry { action } => match action {
            PantryCmd::List => {
                let items = store.list_pantry()?;
                if items.is_empty() {
                    println!("Pantry is empty.");
                } else {
                    for item in items {
                        let unit = ShoppingList::kind_label(item.key.kind);
                        println!(
                            "• {} — {:.1} {} ({:?})",
                            item.key.name, item.quantity_canonical, unit, item.key.kind
                        );
                    }
                }
            }
            PantryCmd::Add { line } => {
                let (key, qty) = parse_pantry_line(&line)?;
                store.pantry_add(&key, qty)?;
                let unit = ShoppingList::kind_label(key.kind);
                println!("Added {qty:.1} {unit} of {} to pantry.", key.name);
                warn_kind_mismatch(&store, &key)?;
            }
            PantryCmd::Set { line } => {
                let (key, qty) = parse_pantry_line(&line)?;
                store.pantry_set(&key, qty)?;
                if qty <= 0.0 {
                    println!("Removed {} from pantry (quantity set to 0).", key.name);
                } else {
                    let unit = ShoppingList::kind_label(key.kind);
                    println!("Set {} to {qty:.1} {unit} in pantry.", key.name);
                    warn_kind_mismatch(&store, &key)?;
                }
            }
            PantryCmd::Remove { name, kind } => {
                let key = resolve_pantry_name(&store, &name, kind)?;
                if store.pantry_remove(&key)? {
                    println!("Removed {} ({:?}) from pantry.", key.name, key.kind);
                } else {
                    println!("Nothing to remove for {}.", key.name);
                }
            }
            PantryCmd::Clear { yes } => {
                if !yes {
                    bail!("refusing to clear pantry without --yes");
                }
                store.pantry_clear()?;
                println!("Pantry cleared.");
            }
            PantryCmd::Restock { plan, catalog } => {
                let plan = resolve_plan(&store, &plan)?;
                let mut cat = PackageCatalog::with_defaults();
                if let Some(path) = catalog {
                    cat.load_json_overlay(&path)
                        .with_context(|| format!("loading catalog {}", path.display()))?;
                }
                // Buy packages then cook the plan: net empty-pantry state is
                // packaging leftover only. Refuses a second restock of the same plan.
                let delta = restock_plan_from_shop(&store, &plan, &cat)?;
                println!(
                    "Restocked plan {}: added {} purchase line(s), deducted {} cooked ingredient(s) \
                     (leftovers remain in pantry).",
                    short_id(&plan.id),
                    delta.additions.len(),
                    delta.deductions.len()
                );
            }
        },
        Commands::Reparse { id, all } => match (all, id) {
            (true, _) => {
                let mut recipes = store.list_recipes(None)?;
                let total = recipes.len();
                eprintln!("Reparsing {total} recipe(s) …");
                for (i, recipe) in recipes.iter_mut().enumerate() {
                    reparse_recipe(recipe);
                    store.save_recipe(recipe)?;
                    eprintln!("  [{}/{}] {}", i + 1, total, recipe.title);
                }
                println!("Reparsed {total} recipe(s).");
            }
            (false, Some(id)) => {
                let mut recipe = resolve_recipe(&store, &id)?;
                reparse_recipe(&mut recipe);
                store.save_recipe(&recipe)?;
                println!(
                    "Reparsed {} ingredient line(s) for {}",
                    recipe.ingredients.len(),
                    recipe.id
                );
            }
            (false, None) => bail!("provide a recipe id or --all"),
        },
    }
    Ok(())
}

/// Re-normalize every ingredient line from its stored original text.
fn reparse_recipe(recipe: &mut Recipe) {
    for line in &mut recipe.ingredients {
        *line = crate::normalize::normalize_line(&line.original);
    }
}

/// Parse a free-text ingredient line into an identity key and canonical quantity.
fn parse_pantry_line(line: &str) -> Result<(IngredientKey, f64)> {
    let parsed = normalize_line(line);
    let key = IngredientKey::from_line(&parsed);
    let Some((qty, _)) = parsed.canonical_quantity() else {
        bail!(
            "could not parse a quantity from '{line}' \
             (use a line like \"2 cups milk\" or \"500g flour\")"
        );
    };
    if !qty.is_finite() || qty <= 0.0 {
        bail!("quantity must be a positive finite number (got {qty})");
    }
    Ok((key, qty))
}

/// Resolve a pantry item by name, optionally disambiguating by unit kind.
fn resolve_pantry_name(store: &Store, name: &str, kind: Option<UnitKind>) -> Result<IngredientKey> {
    let want = crate::domain::normalize_ingredient_name(name);
    let items = store.list_pantry()?;
    let matches: Vec<_> = items
        .into_iter()
        .filter(|p| p.key.name == want)
        .filter(|p| kind.map(|k| p.key.kind == k).unwrap_or(true))
        .collect();
    match matches.as_slice() {
        [] => bail!("no pantry item matching '{name}'"),
        [one] => Ok(one.key.clone()),
        many => {
            let kinds: Vec<_> = many.iter().map(|p| format!("{:?}", p.key.kind)).collect();
            bail!(
                "ambiguous pantry name '{name}' (kinds: {}); pass --kind",
                kinds.join(", ")
            )
        }
    }
}

/// Warn when a pantry key's unit kind differs from how recipes store the same name.
/// Shopping still bridges mass↔volume via density; this makes the trap visible.
fn warn_kind_mismatch(store: &Store, key: &IngredientKey) -> Result<()> {
    let recipes = store.list_recipes(None)?;
    let mut recipe_kinds = HashSet::new();
    for r in &recipes {
        for line in &r.ingredients {
            let k = IngredientKey::from_line(line);
            if k.name == key.name {
                recipe_kinds.insert(k.kind);
            }
        }
    }
    if recipe_kinds.is_empty() {
        return Ok(());
    }
    if !recipe_kinds.contains(&key.kind) {
        let kinds: Vec<_> = recipe_kinds.iter().map(|k| format!("{k:?}")).collect();
        eprintln!(
            "note: recipes use {} as {} — pantry stored as {:?}. \
             Shopping bridges mass↔volume via density when possible; \
             prefer matching the recipe unit (e.g. cups/ml for flour) when unsure.",
            key.name,
            kinds.join("/"),
            key.kind
        );
    }
    Ok(())
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn resolve_recipe(store: &Store, id_or_prefix: &str) -> Result<Recipe> {
    if let Some(r) = store.get_recipe(id_or_prefix)? {
        return Ok(r);
    }
    let all = store.list_recipes(None)?;
    let matches: Vec<_> = all
        .into_iter()
        .filter(|r| r.id.as_str().starts_with(id_or_prefix))
        .collect();
    match matches.len() {
        0 => bail!("no recipe matching '{id_or_prefix}'"),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => {
            bail!("ambiguous recipe id prefix '{id_or_prefix}' ({n} matches); use a longer prefix")
        }
    }
}

fn resolve_plan(store: &Store, id_or_prefix: &str) -> Result<MealPlan> {
    if let Some(p) = store.get_plan(id_or_prefix)? {
        return Ok(p);
    }
    let all = store.list_plans()?;
    let matches: Vec<_> = all
        .into_iter()
        .filter(|p| p.id.starts_with(id_or_prefix))
        .collect();
    match matches.len() {
        0 => bail!("no plan matching '{id_or_prefix}'"),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => bail!("ambiguous plan id prefix '{id_or_prefix}' ({n} matches)"),
    }
}

fn load_pool(store: &Store, pool: Option<&str>) -> Result<Vec<Recipe>> {
    match pool {
        None => store.list_recipes(None),
        Some(s) => {
            let mut out = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for part in s.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let r = resolve_recipe(store, part)?;
                if seen.insert(r.id.clone()) {
                    out.push(r);
                }
            }
            Ok(out)
        }
    }
}

fn print_recipe_summary(r: &Recipe) {
    println!(
        "{} — {} ingredient(s), {} step(s)",
        r.title,
        r.ingredients.len(),
        r.steps.len()
    );
}

fn print_recipe_detail(r: &Recipe) {
    println!("{}", r.title);
    println!("id:       {}", r.id);
    if let Some(s) = r.servings {
        println!("servings: {s}");
    }
    println!("\nIngredients:");
    for line in &r.ingredients {
        let mut s = format!("  - {}", line.original);
        if line.parse_uncertain {
            s.push_str("  [?]");
        }
        println!("{s}");
        if let (Some(q), Some(u)) = (line.quantity, line.unit.as_ref()) {
            println!("      = {q} {} ({:?}) → {}", u.name, u.kind, line.name);
        } else if line.quantity.is_some() {
            println!("      = {:?} → {}", line.quantity, line.name);
        }
    }
    if !r.steps.is_empty() {
        println!("\nSteps:");
        for (i, step) in r.steps.iter().enumerate() {
            println!("  {}. {step}", i + 1);
        }
    }
}

fn print_plan(plan: &MealPlan) {
    println!("Plan {}", plan.id);
    println!(
        "{} day(s), {} meal(s)/day, {} scheduled\n",
        plan.days,
        plan.meals_per_day,
        plan.meals.len()
    );
    let mut current_day = None;
    for m in &plan.meals {
        if current_day != Some(m.day) {
            current_day = Some(m.day);
            println!("Day {}:", m.day + 1);
        }
        println!(
            "  meal {}: {} ({})",
            m.meal + 1,
            m.recipe_title,
            short_id(m.recipe_id.as_str())
        );
    }
    println!("\n{}", plan.rationale);
}

fn print_shopping_list(list: &crate::domain::ShoppingList) {
    println!("Shopping list for plan {}\n", short_id(&list.plan_id));
    for item in &list.items {
        let flag = if item.leftover_flagged {
            " ⚠ leftover"
        } else {
            ""
        };
        println!(
            "• {} — need {:.1} {}",
            item.ingredient.name, item.required_canonical, item.required_unit_label
        );
        for p in &item.packages {
            let price = p
                .unit_price_cents
                .map(|c| format!(" @ ${:.2}/ea", c as f64 / 100.0))
                .unwrap_or_default();
            println!(
                "    buy {} × {} ({:.1} {} each){price}",
                p.count, p.label, p.size_canonical, item.required_unit_label
            );
        }
        println!(
            "    purchase {:.1} {} → leftover {:.1} {}{flag}",
            item.purchased_canonical,
            item.required_unit_label,
            item.leftover_canonical,
            item.required_unit_label
        );
        if let Some(c) = item.total_cost_cents {
            println!("    line cost: ${:.2}", c as f64 / 100.0);
        }
        println!();
    }
    if let Some(t) = list.total_cost_cents {
        println!("Estimated total: ${:.2}", t as f64 / 100.0);
    } else {
        println!("Estimated total: (incomplete pricing)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::IngredientLine;

    #[test]
    fn reparse_renormalizes_from_original() {
        // A line stored with stale parsed fields is re-derived from `original`.
        let mut recipe = Recipe::new("Test");
        recipe.ingredients = vec![IngredientLine {
            original: "¼ cup flour".into(),
            name: "STALE".into(),
            quantity: None,
            unit: None,
            note: None,
            parse_uncertain: true,
        }];
        reparse_recipe(&mut recipe);
        let line = &recipe.ingredients[0];
        assert_eq!(line.original, "¼ cup flour"); // original text preserved
        assert_eq!(line.name, "flour");
        assert_eq!(line.quantity, Some(0.25));
    }

    #[test]
    fn parse_pantry_line_extracts_key_and_canonical_qty() {
        let (key, qty) = parse_pantry_line("2 cups milk").unwrap();
        assert_eq!(key.name, "milk");
        assert_eq!(key.kind, UnitKind::Volume);
        // 2 cups ≈ 473.176 ml
        assert!((qty - 473.176).abs() < 0.1);
    }

    #[test]
    fn parse_pantry_line_rejects_no_quantity() {
        assert!(parse_pantry_line("salt to taste").is_err());
    }
}
