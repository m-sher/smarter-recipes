//! CLI command definitions and handlers.

use crate::domain::{IngredientKey, MealPlan, Recipe, RecipeSource, ShoppingList, UnitKind};
use crate::ingest::{
    ingest_many, normalize_url, read_manual_recipe, recipe_source_url, scrape_new_recipes,
    search_scrape_recipes, HtmlFetcher, HttpFetcher, RecipeSourceIngest, ScrapeEvent,
    ScrapeOutcome, ScrapeParams, UrlSource,
};
use crate::normalize::normalize_line;
use crate::planning::{
    load_nutrition_bounds, plan_bound_violations, plan_meals, plan_tod_mismatches,
    CliPerDayNutrition, PlanOptions, TodMismatch,
};
use crate::pricing::{
    enrich_catalog_from_source, FixtureStoreSource, OpenFoodFactsSource, PackageCatalog,
};
use crate::shopping::{restock_plan_from_shop, shopping_list_for_plan, trip_breakdown_for_plan};
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
    /// Ingest a recipe from a source (file, url, image, epub, auto, or manual).
    /// EPUB imports may save multiple recipes from a linked recipe index.
    Import {
        /// Source kind: file | url | image | epub | auto | manual
        source: String,
        /// Path, URL, image path, or EPUB. Omit for `manual` to enter interactively.
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
        /// Re-attempt URLs recorded as failures
        #[arg(long)]
        retry_failed: bool,
        /// Discover and report without saving
        #[arg(long)]
        dry_run: bool,
    },
    /// Search DuckDuckGo for recipe pages, then multi-host crawl results.
    ///
    /// Works best with **specific dish/ingredient** queries (e.g. "chicken
    /// parmesan recipe"). Broad "best/ideas/high-protein dinners" queries often
    /// hit JS-rendered listicles; static crawl may find few individual recipes.
    SearchScrape {
        /// Search query (prefer a specific dish, e.g. "chicken parmesan recipe")
        query: String,
        /// Max number of site pages to fetch this run (search SERP fetches are free)
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Number of pages to fetch concurrently
        #[arg(long, default_value_t = 8)]
        jobs: usize,
        /// BFS depth from each search result (1 = results only; 2 = one same-host hop)
        #[arg(long, default_value_t = 2)]
        depth: usize,
        /// How many DuckDuckGo result pages to load
        #[arg(long, default_value_t = 2)]
        pages: usize,
        /// Re-attempt URLs recorded as failures
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
    /// Remove non-meal pages (roundups, how-to guides, index/extraction junk).
    /// Dry-run by default; re-run with --apply to delete. Never removes a recipe
    /// that carries usable published nutrition.
    Prune {
        /// Actually delete the listed recipes (default is a dry-run preview).
        #[arg(long)]
        apply: bool,
        /// Comma-separated recipe ids to keep regardless of the check.
        #[arg(long)]
        keep: Option<String>,
    },
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
        /// TOML nutrition bounds (per_day / per_meal / plan scopes)
        #[arg(long)]
        nutrition_config: Option<PathBuf>,
        /// Per-day minimum kcal (overrides config file)
        #[arg(long)]
        min_kcal: Option<f64>,
        /// Per-day maximum kcal (overrides config file)
        #[arg(long)]
        max_kcal: Option<f64>,
        /// Per-day minimum protein grams (overrides config file)
        #[arg(long)]
        min_protein_g: Option<f64>,
        /// Per-day maximum protein grams (overrides config file)
        #[arg(long)]
        max_protein_g: Option<f64>,
        /// Per-day minimum fat grams (overrides config file)
        #[arg(long)]
        min_fat_g: Option<f64>,
        /// Per-day maximum fat grams (overrides config file)
        #[arg(long)]
        max_fat_g: Option<f64>,
        /// Per-day minimum carbs grams (overrides config file)
        #[arg(long)]
        min_carbs_g: Option<f64>,
        /// Per-day maximum carbs grams (overrides config file)
        #[arg(long)]
        max_carbs_g: Option<f64>,
        /// Steer slots toward breakfast/lunch/dinner using recipe tags and
        /// schema.org categories (soft mismatches; see rationale)
        #[arg(long)]
        tod: bool,
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
    /// Re-parse stored ingredient lines with the current parser. Pass a recipe
    /// id, or `--all` to reparse every recipe.
    Reparse {
        /// Recipe id (prefix match). Omit when using --all.
        id: Option<String>,
        /// Reparse every stored recipe
        #[arg(long)]
        all: bool,
    },
    /// Re-fetch URL-sourced recipes from their source and update them in place,
    /// backfilling schema.org category and re-parsing with the current parser.
    /// Dry-run by default; re-run with --apply to fetch and write changes.
    Refresh {
        /// Recipe id (prefix match). Omit when using --all.
        id: Option<String>,
        /// Refresh every URL-sourced recipe
        #[arg(long)]
        all: bool,
        /// Actually fetch and write changes (default: dry-run preview)
        #[arg(long)]
        apply: bool,
        /// Concurrent fetches
        #[arg(long, default_value_t = 6)]
        jobs: usize,
    },
    /// Label missing recipe categories with a cheap Gemini model so the plan
    /// category filter can keep meals and drop drinks/components.
    ///
    /// Dry-run by default (counts + optional live sample). Re-run with `--apply`
    /// to write. Prefer `refresh --all --apply` first for URL recipes that publish
    /// schema.org categories; this fills the rest (especially EPUB imports).
    ///
    /// API key: `SMARTER_RECIPES_GEMINI_API_KEY` or `GEMINI_API_KEY` (or `--api-key`).
    Categorize {
        /// Optional recipe id prefix (must be unique among eligible recipes)
        id: Option<String>,
        /// Write labels to the database (default: dry-run preview)
        #[arg(long)]
        apply: bool,
        /// Re-label recipes that already have a category
        #[arg(long)]
        force: bool,
        /// Max recipes to process this run
        #[arg(long)]
        limit: Option<usize>,
        /// Recipes per Gemini request
        #[arg(long, default_value_t = crate::categorize::DEFAULT_BATCH_SIZE)]
        batch_size: usize,
        /// Gemini model id
        #[arg(long, default_value = crate::categorize::DEFAULT_MODEL)]
        model: String,
        /// Restrict by source: all | epub | url
        #[arg(long, default_value = "all")]
        source: String,
        /// Dry-run: live-label this many recipes as a sample (0 = count only, no network)
        #[arg(long, default_value_t = crate::categorize::DEFAULT_SAMPLE)]
        sample: usize,
        /// Skip confirmation when applying more than 100 labels
        #[arg(long)]
        yes: bool,
        /// Gemini API key (otherwise env)
        #[arg(long, env = "SMARTER_RECIPES_GEMINI_API_KEY")]
        api_key: Option<String>,
    },
    /// Track on-hand ingredients (pantry stock)
    Pantry {
        #[command(subcommand)]
        action: PantryCmd,
    },
    /// Nutrition data management
    Nutrition {
        #[command(subcommand)]
        action: NutritionCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum NutritionCmd {
    /// Resolve per-100 g profiles for ingredients not covered by the built-in
    /// table, caching results (USDA FoodData Central, or an offline fixture)
    Fetch {
        /// JSON fixture file (name -> {kcal, protein_g, fat_g, carbs_g}) instead of the network
        #[arg(long)]
        fixture: Option<PathBuf>,
        /// Max lookups this run
        #[arg(long, default_value_t = 25)]
        limit: usize,
        /// Concurrent lookups (a shared per-source rate gate caps the request rate)
        #[arg(long, default_value_t = 6)]
        jobs: usize,
    },
    /// Clear the cached nutrition lookups (positive and negative), forcing a
    /// re-fetch on the next `nutrition fetch`
    ClearCache,
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
            let input_path = input.clone();
            let batch = if interactive {
                crate::ingest::IngestBatch::recipes_only(vec![read_manual_recipe(
                    &mut std::io::stdin().lock(),
                    &mut std::io::stderr(),
                )?])
            } else {
                let input = input
                    .ok_or_else(|| anyhow::anyhow!("input is required for source '{source}'"))?;
                ingest_many(&source, &input)?
            };
            let recipes = batch.recipes;
            let skipped_ambiguous = batch.skipped_ambiguous.len();
            let multi = recipes.len() > 1
                || skipped_ambiguous > 0
                || matches!(source.to_lowercase().as_str(), "epub" | "ebook")
                || input_path.as_deref().is_some_and(|p| {
                    std::path::Path::new(p)
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("epub"))
                });
            let mut saved = 0usize;
            let mut skipped_dup = 0usize;
            let mut skipped_junk = 0usize;
            for mut recipe in recipes {
                recipe.title = crate::text::sanitize(&recipe.title);
                if !multi {
                    print_recipe_summary(&recipe);
                } else {
                    eprintln!(
                        "  · {} ({} ingredients)",
                        recipe.title,
                        recipe.ingredients.len()
                    );
                }
                if dry_run {
                    println!("{}", serde_json::to_string_pretty(&recipe)?);
                    continue;
                }
                if store.is_duplicate(recipe_source_url(&recipe).as_deref())? {
                    if multi {
                        skipped_dup += 1;
                    } else {
                        println!(
                            "Skipped: already have a recipe with this source URL ({})",
                            recipe.title
                        );
                    }
                } else if prunable(&recipe) {
                    if multi {
                        skipped_junk += 1;
                    } else {
                        println!(
                            "Skipped: not a cookable recipe and no published nutrition — {} ingredient line(s), too few carry amounts ({})",
                            recipe.ingredients.len(),
                            recipe.title
                        );
                    }
                } else {
                    store.save_recipe(&recipe)?;
                    if multi {
                        saved += 1;
                    } else {
                        println!("Saved recipe {} to {}", recipe.id, store.path().display());
                    }
                }
            }
            if multi && !dry_run {
                println!(
                    "{}",
                    format_batch_import_summary(
                        saved,
                        skipped_dup,
                        skipped_junk,
                        skipped_ambiguous,
                        &store.path().display().to_string(),
                    )
                );
            } else if multi && dry_run {
                println!("(dry run) nothing saved (would skip {skipped_ambiguous} ambiguous)");
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
            let skip = scrape_skip_set(&store, retry_failed)?;
            eprintln!("Scanning {url} (depth {depth}) …");
            let fetcher = HttpFetcher::default();
            let outcome = scrape_new_recipes(
                &fetcher,
                &url,
                limit,
                &skip,
                jobs,
                depth,
                &scrape_progress_printer,
            )?;
            apply_scrape_outcome(&store, &outcome, dry_run)?;
        }
        Commands::SearchScrape {
            query,
            limit,
            jobs,
            depth,
            pages,
            retry_failed,
            dry_run,
        } => {
            let skip = scrape_skip_set(&store, retry_failed)?;
            eprintln!(
                "Searching DuckDuckGo for {query:?} ({pages} result page(s), depth {depth}, limit {limit}) …"
            );
            let fetcher = HttpFetcher::default();
            let outcome = search_scrape_recipes(
                &fetcher,
                &query,
                &skip,
                ScrapeParams::new(limit, jobs, depth),
                pages,
                &scrape_progress_printer,
            )?;
            apply_scrape_outcome(&store, &outcome, dry_run)?;
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
        Commands::Prune { apply, keep } => {
            let keep: std::collections::HashSet<String> = keep
                .as_deref()
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            let recipes = store.list_recipes(None)?;
            // Purge a recipe only if it is NOT a cookable meal AND carries no
            // usable source nutrition.
            let candidates: Vec<&Recipe> = recipes
                .iter()
                // Prefix match against the recipe id.
                .filter(|r| !keep.iter().any(|k| r.id.as_str().starts_with(k)) && prunable(r))
                .collect();
            if candidates.is_empty() {
                println!("No non-meals to prune.");
            } else if apply {
                let mut deleted = 0usize;
                for r in &candidates {
                    if store.delete_recipe(r.id.as_str())? {
                        deleted += 1;
                    }
                }
                println!("Pruned {deleted} non-meal recipe(s).");
            } else {
                println!(
                    "{} non-meal recipe(s) would be pruned (dry run — re-run with --apply to delete):\n",
                    candidates.len()
                );
                for r in &candidates {
                    println!(
                        "  {}  {:<48}  {} ingredient line(s)",
                        short_id(r.id.as_str()),
                        r.title.chars().take(48).collect::<String>(),
                        r.ingredients.len(),
                    );
                }
                println!("\nWhitelist any genuine recipes with --keep <id,id,…>, then re-run with --apply.");
            }
        }
        Commands::Plan {
            days,
            per_day,
            pool,
            nutrition_config,
            min_kcal,
            max_kcal,
            min_protein_g,
            max_protein_g,
            min_fat_g,
            max_fat_g,
            min_carbs_g,
            max_carbs_g,
            tod,
            json,
            dry_run,
        } => {
            let recipes = load_pool(&store, pool.as_deref())?;
            if recipes.is_empty() {
                bail!("recipe pool is empty; import recipes first");
            }
            let pantry = store.list_pantry()?;
            let cli_nutrition = CliPerDayNutrition {
                min_kcal,
                max_kcal,
                min_protein_g,
                max_protein_g,
                min_fat_g,
                max_fat_g,
                min_carbs_g,
                max_carbs_g,
            };
            let nutrition = load_nutrition_bounds(nutrition_config.as_deref(), &cli_nutrition)?;
            let extra = nutrition_extra(&store)?;
            let (recipe_macros, recipe_low_coverage) = recipe_macros_for_pool(&recipes, &extra);
            let opts = PlanOptions {
                days,
                meals_per_day: per_day,
                pantry,
                nutrition: nutrition.clone(),
                recipe_macros: recipe_macros.clone(),
                recipe_low_coverage,
                time_of_day: tod,
            };
            let plan = plan_meals(&recipes, &opts);
            let tod_misses = if tod {
                plan_tod_mismatches(&recipes, &plan)
            } else {
                Vec::new()
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                print_plan(&plan);
                match crate::nutrition::plan_nutrition(&store, &plan, &extra) {
                    Ok(pn) => print_plan_nutrition(&pn),
                    Err(e) => eprintln!("note: nutrition estimate unavailable: {e:#}"),
                }
                if !nutrition.is_empty() {
                    let violations =
                        plan_bound_violations(&recipes, &plan, &nutrition, &recipe_macros);
                    print_plan_constraints(&violations);
                }
                if tod {
                    print_plan_tod(&tod_misses);
                }
            }
            // Emit regardless of --json so machine consumers still see soft misses
            // (structured per-slot detail remains in the rationale / human summary).
            if tod && !tod_misses.is_empty() {
                eprintln!(
                    "warning: time-of-day: {} slot(s) could not be matched to labeled recipes",
                    tod_misses.len()
                );
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
                for recipe in recipes.iter_mut() {
                    reparse_recipe(recipe);
                }
                store.save_recipes(&recipes, |done| {
                    eprintln!("  reparsed {done}/{total}");
                })?;
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
        Commands::Refresh {
            id,
            all,
            apply,
            jobs,
        } => {
            let recipes = store.list_recipes(None)?;
            let selected: Vec<&Recipe> = match (all, &id) {
                (true, _) => recipes.iter().collect(),
                (false, Some(pfx)) => {
                    let v: Vec<&Recipe> = recipes
                        .iter()
                        .filter(|r| r.id.as_str().starts_with(pfx.as_str()))
                        .collect();
                    // Mirror resolve_recipe: a prefix must identify one recipe.
                    match v.len() {
                        0 => bail!("no recipe matches id '{pfx}'"),
                        1 => v,
                        n => bail!(
                            "ambiguous recipe id prefix '{pfx}' matches {n} recipes; use a longer prefix or --all"
                        ),
                    }
                }
                (false, None) => bail!("provide a recipe id or --all"),
            };
            // Only URL-sourced recipes can be re-fetched; report the rest.
            let mut targets: Vec<(&Recipe, String)> = Vec::new();
            let mut skipped_non_url = 0usize;
            for r in &selected {
                match &r.source {
                    RecipeSource::Url { url } => targets.push((r, url.clone())),
                    _ => skipped_non_url += 1,
                }
            }
            if targets.is_empty() {
                println!(
                    "No URL-sourced recipes to refresh ({skipped_non_url} without a source URL)."
                );
            } else if apply {
                let fetcher = HttpFetcher::default();
                eprintln!("Refreshing {} URL-sourced recipe(s) …", targets.len());
                let report = refresh_recipes(&store, &fetcher, &targets, jobs.max(1), true)?;
                println!(
                    "Refreshed {} recipe(s) ({} now carry a category); \
                     {} fetch failure(s), {} parse failure(s), {} save failure(s), \
                     {skipped_non_url} without a source URL skipped.",
                    report.updated,
                    report.with_category,
                    report.fetch_failed,
                    report.parse_failed,
                    report.save_failed,
                );
            } else {
                println!(
                    "{} URL-sourced recipe(s) would be refreshed (dry run — re-run with --apply):\n",
                    targets.len()
                );
                for (r, url) in &targets {
                    println!(
                        "  {}  {:<40}  {url}",
                        short_id(r.id.as_str()),
                        r.title.chars().take(40).collect::<String>(),
                    );
                }
                if skipped_non_url > 0 {
                    println!("\n{skipped_non_url} recipe(s) without a source URL will be skipped.");
                }
                println!("\nRe-run with --apply to fetch and update in place.");
            }
        }
        Commands::Categorize {
            id,
            apply,
            force,
            limit,
            batch_size,
            model,
            source,
            sample,
            yes,
            api_key,
        } => {
            use crate::categorize::{
                resolve_api_key, run_categorize, CategorizeOptions, GeminiLabeler, SourceFilter,
            };
            let source = SourceFilter::parse(&source)?;
            let opts = CategorizeOptions {
                apply,
                force,
                limit,
                batch_size: batch_size.max(1),
                sample,
                source,
                yes,
                id_prefix: id,
            };

            // Need a key for apply, or dry-run with sample > 0.
            let needs_network = apply || opts.sample > 0;
            let key = if needs_network {
                Some(resolve_api_key(api_key.as_deref())?)
            } else {
                // Still allow resolve if provided; optional for count-only dry-run.
                resolve_api_key(api_key.as_deref()).ok()
            };

            let all = store.list_recipes(None)?;
            if let Some(pfx) = &opts.id_prefix {
                let matches: Vec<_> = all
                    .iter()
                    .filter(|r| r.id.as_str().starts_with(pfx.as_str()))
                    .collect();
                match matches.len() {
                    0 => bail!("no recipe matches id '{pfx}'"),
                    1 => {}
                    n => bail!(
                        "ambiguous recipe id prefix '{pfx}' matches {n} recipes; use a longer prefix"
                    ),
                }
            }
            let (eligible, skipped_labeled) = crate::categorize::select_eligible(&all, &opts);
            let batch = opts.batch_size.max(1);
            let batches = if eligible.is_empty() {
                0
            } else {
                eligible.len().div_ceil(batch)
            };

            let mode = if apply { "apply" } else { "dry-run" };
            println!(
                "Categorize ({mode}): {} eligible, source={}, model={model}",
                eligible.len(),
                source.as_str(),
            );
            if skipped_labeled > 0 && !force {
                println!("  Skipping {skipped_labeled} already labeled (use --force to re-label)");
            }
            if !eligible.is_empty() {
                println!("  Plan: ~{batches} API batch(es) of up to {batch} recipe(s)");
            }

            if eligible.is_empty() {
                println!("Nothing to categorize.");
            } else if !needs_network {
                println!(
                    "\nCount-only dry-run (no network). Re-run with default --sample to preview \
                     labels, or with --apply to write. Tip: start with --limit 50 --apply."
                );
            } else {
                let key = key.expect("key required when needs_network");
                let labeler = GeminiLabeler::new(key, &model)?;
                let report = run_categorize(&store, &labeler, &opts)?;
                if apply {
                    println!(
                        "Done: labeled {}, left empty {} (low confidence), \
                         failed batches {}, written {}.",
                        report.labeled, report.left_empty, report.failed_batches, report.written
                    );
                    if report.failed_batches > 0 {
                        bail!(
                            "{} batch(es) failed; re-run to retry remaining unlabeled recipes",
                            report.failed_batches
                        );
                    }
                } else {
                    println!(
                        "\nRe-run with --apply to write categories. \
                         Use --limit 50 for a small first pass."
                    );
                }
            }
        }
        Commands::Nutrition { action } => match action {
            NutritionCmd::Fetch {
                fixture,
                limit,
                jobs,
            } => {
                let jobs = jobs.max(1);
                let extra = nutrition_extra(&store)?;
                let cache = store.nutrition_cache_all()?;
                // Distinct ingredient names with no macro profile yet.
                let mut names: Vec<String> = std::collections::BTreeSet::from_iter(
                    store
                        .list_recipes(None)?
                        .iter()
                        .flat_map(|r| r.ingredients.iter())
                        .filter(|l| l.canonical_quantity().is_some())
                        .map(|l| IngredientKey::from_line(l).name),
                )
                .into_iter()
                .filter(|n| {
                    !crate::nutrition::is_probable_junk_name(n)
                        && crate::nutrition::resolve_profile(n, &extra).is_none()
                })
                .collect();
                // Network runs skip already-cached names; a fixture re-checks
                // every name and never writes misses.
                let from_network = fixture.is_none();
                if from_network {
                    names.retain(|n| !cache.contains_key(n));
                }
                if names.is_empty() {
                    println!("All uncovered ingredient names have been looked up already.");
                    return Ok(());
                }
                let (source, source_label): (Box<dyn crate::nutrition::NutritionSource>, String) =
                    match &fixture {
                        Some(path) => (
                            Box::new(crate::nutrition::FixtureNutritionSource::from_path(path)?),
                            "fixture".to_string(),
                        ),
                        // Chain providers: fall back to keyless Open Food Facts.
                        None => {
                            let chain = crate::nutrition::ChainedNutritionSource::new(vec![
                                Box::new(crate::nutrition::FdcSource::default()),
                                Box::new(crate::nutrition::OpenFoodFactsNutritionSource::default()),
                            ]);
                            let label = chain.source_names();
                            (Box::new(chain), label)
                        }
                    };
                eprintln!(
                    "Resolving {} of {} uncovered ingredient name(s) via {source_label} …",
                    names.len().min(limit),
                    names.len(),
                );
                let mut found = 0usize;
                let mut missed = 0usize;
                let mut errored = 0usize;
                let mut rate_limited = false;
                let selected: Vec<&String> = names.iter().take(limit).collect();
                let total = selected.len();
                let source_ref: &dyn crate::nutrition::NutritionSource = &*source;
                let mut done = 0usize;
                'batches: for batch in selected.chunks(jobs) {
                    // Look up this batch concurrently. Store writes and progress
                    // stay on the main thread; the sources' shared rate gate keeps
                    // the real request rate in check regardless of `jobs`.
                    type Row = (usize, Result<Option<crate::domain::Macros>>);
                    let results: std::sync::Mutex<Vec<Row>> =
                        std::sync::Mutex::new(Vec::with_capacity(batch.len()));
                    std::thread::scope(|scope| {
                        for (bi, &name) in batch.iter().enumerate() {
                            let results = &results;
                            scope.spawn(move || {
                                let r = source_ref.lookup(name);
                                results.lock().unwrap().push((bi, r));
                            });
                        }
                    });
                    let mut rows = results.into_inner().unwrap();
                    rows.sort_by_key(|(bi, _)| *bi);
                    for (bi, r) in rows {
                        let name = batch[bi];
                        done += 1;
                        match r {
                            Ok(Some(profile)) => {
                                store.nutrition_cache_put(name, Some(&profile))?;
                                eprintln!(
                                    "  [{done}/{total}] + {name}  ({:.0} kcal/100g)",
                                    profile.kcal
                                );
                                found += 1;
                            }
                            Ok(None) => {
                                if from_network {
                                    store.nutrition_cache_put(name, None)?;
                                }
                                eprintln!("  [{done}/{total}] - {name}  (no match)");
                                missed += 1;
                            }
                            Err(e) => {
                                // On rate limit, finish this batch's good results, then stop.
                                if e.downcast_ref::<crate::nutrition::RateLimited>().is_some() {
                                    eprintln!("  [{done}/{total}] ! stopping: {e:#}");
                                    rate_limited = true;
                                    continue;
                                }
                                eprintln!("  [{done}/{total}] ! {name}  ({e:#})");
                                errored += 1;
                            }
                        }
                    }
                    if rate_limited {
                        break 'batches;
                    }
                }
                // Remaining = names never resolved: those past the limit plus
                // any that errored.
                let remaining = names.len() - found - missed;
                println!(
                    "Cached {found} profile(s), {missed} miss(es), {errored} error(s); \
                     {remaining} name(s) remaining."
                );
                if rate_limited {
                    println!(
                        "Stopped early due to rate limiting; rerun `nutrition fetch` later to continue."
                    );
                }
            }
            NutritionCmd::ClearCache => {
                let n = store.nutrition_cache_clear()?;
                println!("Cleared {n} cached nutrition entr(ies).");
            }
        },
    }
    Ok(())
}

fn scrape_skip_set(store: &Store, retry_failed: bool) -> Result<HashSet<String>> {
    let mut skip: HashSet<String> = store
        .list_recipes(None)?
        .iter()
        .filter_map(recipe_source_url)
        .map(|u| normalize_url(&u))
        .collect();
    if !retry_failed {
        skip.extend(store.failed_scrape_urls()?);
    }
    Ok(skip)
}

fn scrape_progress_printer(event: ScrapeEvent) {
    match event {
        ScrapeEvent::Planned {
            candidates,
            skipped,
            to_fetch,
        } => eprintln!(
            "Found {candidates} candidate link(s); {skipped} already known; queue {to_fetch} …"
        ),
        ScrapeEvent::Imported { url, title } => eprintln!("  ✓ {title}  ({url})"),
        ScrapeEvent::NotRecipe { url, reason } => {
            eprintln!("  · nav {url}  ({reason})")
        }
        ScrapeEvent::Failed { url, reason } => {
            eprintln!("  ✗ fetch {url}  ({reason})")
        }
    }
}

fn apply_scrape_outcome(store: &Store, outcome: &ScrapeOutcome, dry_run: bool) -> Result<()> {
    let mut skipped_dup = 0usize;
    let mut skipped_noncookable = 0usize;
    let mut saved = 0usize;
    for recipe in &outcome.recipes {
        let source = recipe_source_url(recipe);
        if store.is_duplicate(source.as_deref())? {
            skipped_dup += 1;
            continue;
        }
        // Keep roundups / index pages / how-to guides out of the catalog, but
        // admit an amount-sparse page that publishes usable nutrition.
        if prunable(recipe) {
            skipped_noncookable += 1;
            continue;
        }
        if !dry_run {
            store.save_recipe(recipe)?;
            if let Some(u) = source {
                store.clear_scrape_failure(&normalize_url(&u))?;
            }
        }
        saved += 1;
    }
    if !dry_run {
        for (link, reason) in &outcome.failed {
            store.record_scrape_failure(&normalize_url(link), reason)?;
        }
    }

    if dry_run {
        println!(
            "(dry run) {} new recipe(s), {} nav (not recipe), {} non-cookable (roundup/guide), {} fetch failed, {} skipped known URL, {} would skip URL/title dup — nothing saved",
            saved,
            outcome.not_recipe.len(),
            skipped_noncookable,
            outcome.failed.len(),
            outcome.skipped_existing,
            skipped_dup
        );
    } else {
        println!(
            "Imported {} new recipe(s) to {} ({} nav, {} non-cookable, {} fetch failed, {} skipped known URL, {} skipped URL/title dup)",
            saved,
            store.path().display(),
            outcome.not_recipe.len(),
            skipped_noncookable,
            outcome.failed.len(),
            outcome.skipped_existing,
            skipped_dup
        );
    }
    Ok(())
}

/// Cache-backed extra profiles (positive entries only) for nutrition math.
fn nutrition_extra(
    store: &Store,
) -> Result<std::collections::HashMap<String, crate::domain::Macros>> {
    Ok(store
        .nutrition_cache_all()?
        .into_iter()
        .filter_map(|(k, v)| v.map(|m| (k, m)))
        .collect())
}

/// Whole-recipe macros for each recipe, plus the set whose data can't be trusted
/// against a nutrition bound (the planner drops those when bounds are set).
///
/// Prefers the source page's published macros when present and plausible.
/// Otherwise falls back to the ingredient estimate, and flags a recipe
/// low-coverage when fewer than [`crate::planning::MIN_INGREDIENT_COVERAGE`] of
/// its estimable ingredients resolve.
fn recipe_macros_for_pool(
    recipes: &[Recipe],
    extra: &std::collections::HashMap<String, crate::domain::Macros>,
) -> (
    std::collections::HashMap<crate::domain::RecipeId, crate::domain::Macros>,
    std::collections::HashSet<crate::domain::RecipeId>,
) {
    let mut macros = std::collections::HashMap::new();
    let mut low_coverage = std::collections::HashSet::new();
    for r in recipes {
        let n = crate::nutrition::recipe_nutrition(r, extra);
        // Coverage over estimable (quantity-bearing) ingredients only; a recipe
        // with none (all "to taste") is left to the zero-kcal/no-macro filter.
        let estimable = n.covered.len() + n.uncovered.len();
        let coverage = if estimable > 0 {
            n.covered.len() as f64 / estimable as f64
        } else {
            0.0
        };

        // Prefer authoritative source macros when present and internally plausible
        // (validated inside source_recipe_macros: whole-recipe detection, absolute
        // ceilings, Atwater consistency). Does not cross-check against the
        // ingredient estimate.
        if let Some(src) = crate::nutrition::source_recipe_macros(r) {
            macros.insert(r.id.clone(), src); // authoritative; never low-coverage
        } else {
            if estimable > 0 && coverage < crate::planning::MIN_INGREDIENT_COVERAGE {
                low_coverage.insert(r.id.clone());
            }
            macros.insert(r.id.clone(), n.macros);
        }
    }
    (macros, low_coverage)
}

/// Final one-line tally for multi-recipe import (includes EPUB ambiguous skips).
fn format_batch_import_summary(
    saved: usize,
    skipped_dup: usize,
    skipped_junk: usize,
    skipped_ambiguous: usize,
    store_path: &str,
) -> String {
    format!(
        "Batch import: saved {saved}, skipped {skipped_dup} duplicate(s), skipped {skipped_junk} non-cookable, skipped {skipped_ambiguous} ambiguous → {store_path}"
    )
}

/// True when a recipe should be pruned as a non-meal: it is not structurally
/// cookable AND carries no usable source nutrition.
fn prunable(recipe: &Recipe) -> bool {
    !crate::ingest::is_cookable(recipe) && crate::nutrition::source_recipe_macros(recipe).is_none()
}

fn print_plan_constraints(violations: &[crate::planning::BoundViolation]) {
    println!("\nNutrition constraints:");
    if violations.is_empty() {
        println!("  All configured bounds satisfied.");
        return;
    }
    println!("  Best effort; {} bound(s) not met:", violations.len());
    for v in violations {
        println!("  - {v}");
    }
}

fn print_plan_tod(misses: &[TodMismatch]) {
    println!("\nTime of day:");
    if misses.is_empty() {
        println!("  All slots matched.");
        return;
    }
    println!("  Best effort; {} slot(s) mismatched:", misses.len());
    for m in misses {
        println!(
            "  - day {} meal {}: expected {}, got {} ({})",
            m.day + 1,
            m.meal + 1,
            m.expected,
            m.recipe_title,
            m.labels.describe()
        );
    }
}

fn print_plan_nutrition(pn: &crate::nutrition::PlanNutrition) {
    if pn.per_day.is_empty() {
        return;
    }
    // Totals match the planner's bounds: published source macros where available,
    // the ingredient estimate otherwise.
    println!("\nNutrition (whole recipes, per day):");
    for (day, m) in &pn.per_day {
        println!(
            "  Day {}: {:.0} kcal | protein {:.0} g | fat {:.0} g | carbs {:.0} g",
            day + 1,
            m.kcal,
            m.protein_g,
            m.fat_g,
            m.carbs_g
        );
    }

    // Provenance note: how many meals used the source's published nutrition, and
    // the ingredient-estimate coverage for the rest.
    let counted = pn.covered.len() + pn.uncovered.len();
    let mut notes: Vec<String> = Vec::new();
    if pn.source_backed > 0 {
        notes.push(format!(
            "{} recipe(s) use the source's published nutrition",
            pn.source_backed
        ));
    }
    if counted > 0 {
        let mut c = format!("{}/{} ingredients estimated", pn.covered.len(), counted);
        if !pn.uncovered.is_empty() {
            let sample: Vec<&str> = pn.uncovered.iter().map(String::as_str).take(4).collect();
            let more = pn.uncovered.len().saturating_sub(sample.len());
            c.push_str(&format!(
                " (uncovered: {}{})",
                sample.join(", "),
                if more > 0 {
                    format!(", +{more} more")
                } else {
                    String::new()
                }
            ));
        }
        notes.push(c);
    }
    if !notes.is_empty() {
        let mut line = format!("  {}", notes.join("; "));
        // Only suggest fetching for names a fetch could resolve (missing profile).
        if !pn.fetchable.is_empty() {
            line.push_str("; run `nutrition fetch` to resolve missing profiles");
        }
        println!("{line}");
    }
}

/// Re-normalize every ingredient line from its stored original text.
fn reparse_recipe(recipe: &mut Recipe) {
    recipe.title = crate::text::sanitize(&recipe.title);
    for line in &mut recipe.ingredients {
        *line = crate::normalize::normalize_line(&line.original);
    }
}

#[derive(Default)]
struct RefreshReport {
    updated: usize,
    with_category: usize,
    fetch_failed: usize,
    parse_failed: usize,
    save_failed: usize,
}

/// Re-fetch each `(existing recipe, url)` target, re-parse the HTML offline, and
/// save the result in place — preserving the existing id and original source.
/// Fetches run `jobs`-concurrently (store writes stay on the calling thread). A
/// fetch or parse failure leaves the existing row untouched.
fn refresh_recipes(
    store: &Store,
    fetcher: &dyn HtmlFetcher,
    targets: &[(&Recipe, String)],
    jobs: usize,
    verbose: bool,
) -> Result<RefreshReport> {
    let total = targets.len();
    let mut report = RefreshReport::default();
    let mut done = 0usize;
    for batch in targets.chunks(jobs.max(1)) {
        type Row = (usize, Result<String>);
        let results: std::sync::Mutex<Vec<Row>> =
            std::sync::Mutex::new(Vec::with_capacity(batch.len()));
        std::thread::scope(|scope| {
            for (bi, (_r, url)) in batch.iter().enumerate() {
                let results = &results;
                scope.spawn(move || {
                    let fetched = fetcher.fetch(url);
                    results.lock().unwrap().push((bi, fetched));
                });
            }
        });
        let mut rows = results.into_inner().unwrap();
        rows.sort_by_key(|(bi, _)| *bi);
        for (bi, fetched) in rows {
            let (existing, url) = &batch[bi];
            done += 1;
            let html = match fetched {
                Ok(html) => html,
                Err(e) => {
                    report.fetch_failed += 1;
                    if verbose {
                        eprintln!(
                            "  [{done}/{total}] ! fetch failed: {} ({e:#})",
                            existing.title
                        );
                    }
                    continue;
                }
            };
            let parsed = UrlSource {
                offline_html: Some(html),
                ..Default::default()
            }
            .ingest(url);
            match parsed {
                Ok(mut fresh) => {
                    // Adopt the existing identity and keep the original source URL
                    // on both the source and the provenance field; clean the title.
                    fresh.title = crate::text::sanitize(&fresh.title);
                    fresh.id = existing.id.clone();
                    fresh.source = existing.source.clone();
                    fresh.meta.source_url = existing.meta.source_url.clone();
                    let category = fresh.meta.category.clone();
                    // Count a save failure and move on.
                    match store.save_recipe(&fresh) {
                        Ok(()) => {
                            report.updated += 1;
                            if category.is_some() {
                                report.with_category += 1;
                            }
                            if verbose {
                                match &category {
                                    Some(c) => {
                                        eprintln!("  [{done}/{total}] + {}  [{c}]", fresh.title)
                                    }
                                    None => eprintln!(
                                        "  [{done}/{total}] + {}  (no category)",
                                        fresh.title
                                    ),
                                }
                            }
                        }
                        Err(e) => {
                            report.save_failed += 1;
                            if verbose {
                                eprintln!(
                                    "  [{done}/{total}] ! save failed: {} ({e:#})",
                                    fresh.title
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    report.parse_failed += 1;
                    if verbose {
                        eprintln!(
                            "  [{done}/{total}] ! parse failed: {} ({e:#})",
                            existing.title
                        );
                    }
                }
            }
        }
    }
    Ok(report)
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
    if let Some(c) = &r.meta.category {
        println!("category: {c}");
    }
    if let Some(s) = r.servings {
        println!("servings: {s}");
    }
    if let Some(n) = &r.meta.nutrition {
        let fmt = |v: Option<f64>, unit: &str| {
            v.map(|x| format!("{x:.0} {unit}"))
                .unwrap_or_else(|| "?".into())
        };
        println!(
            "nutrition (per serving, as published): {} | protein {} | fat {} | carbs {}",
            fmt(n.kcal, "kcal"),
            fmt(n.protein_g, "g"),
            fmt(n.fat_g, "g"),
            fmt(n.carbs_g, "g")
        );
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
        let star = if m.uses_pantry { " ★" } else { "" };
        println!(
            "  meal {}: {}{star} ({})",
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
    fn batch_import_summary_includes_ambiguous_count() {
        let line = format_batch_import_summary(1, 0, 0, 3, "/tmp/recipes.db");
        assert_eq!(
            line,
            "Batch import: saved 1, skipped 0 duplicate(s), skipped 0 non-cookable, skipped 3 ambiguous → /tmp/recipes.db"
        );
        assert!(line.contains("skipped 3 ambiguous"));
    }

    #[test]
    fn recipe_macros_for_pool_gates_on_coverage_ratio() {
        use crate::domain::{Macros, RecipeId};
        let profile = Macros {
            kcal: 100.0,
            protein_g: 5.0,
            fat_g: 2.0,
            carbs_g: 10.0,
        };
        let mk = |id: &str, ings: &[&str]| {
            let mut r = Recipe::new(id);
            r.id = RecipeId::from(id);
            r.ingredients = ings.iter().map(|l| normalize_line(l)).collect();
            r
        };
        // Nonsense ingredient names with mass quantities.
        let recipes = vec![
            mk(
                "ninety",
                &[
                    "100 g zaa",
                    "100 g zab",
                    "100 g zac",
                    "100 g zad",
                    "100 g zae",
                    "100 g zaf",
                    "100 g zag",
                    "100 g zah",
                    "100 g zai",
                    "100 g zzz",
                ],
            ),
            mk("half", &["100 g zaa", "100 g zzz"]),
        ];
        let extra: std::collections::HashMap<String, Macros> = [
            "zaa", "zab", "zac", "zad", "zae", "zaf", "zag", "zah", "zai",
        ]
        .iter()
        .map(|n| (n.to_string(), profile))
        .collect();

        let (_macros, low) = recipe_macros_for_pool(&recipes, &extra);
        // 9/10 = 90% meets the threshold → kept.
        assert!(!low.contains(&RecipeId::from("ninety")));
        // 1/2 = 50% → excluded.
        assert!(low.contains(&RecipeId::from("half")));
    }

    #[test]
    fn recipe_macros_for_pool_prefers_source_nutrition() {
        use crate::domain::{Nutrition, RecipeId};
        // All-nonsense ingredients → near-zero estimate coverage; the source
        // page carries published macros.
        let mut r = Recipe::new("Mystery Stew");
        r.id = RecipeId::from("stew");
        r.ingredients = ["2 cups chopped zaa", "1 clove zbb", "3 blorps zcc"]
            .iter()
            .map(|l| normalize_line(l))
            .collect();
        r.servings = Some(4.0);
        r.meta.nutrition = Some(Nutrition {
            kcal: Some(300.0),
            protein_g: Some(20.0),
            fat_g: Some(10.0),
            carbs_g: Some(30.0),
        });
        let (macros, low) = recipe_macros_for_pool(&[r], &std::collections::HashMap::new());
        let id = RecipeId::from("stew");
        assert!(
            !low.contains(&id),
            "a source-backed recipe must not be flagged low-coverage"
        );
        // Whole-recipe macros = per-serving × servings (300 × 4).
        assert!(
            (macros[&id].kcal - 1200.0).abs() < 1e-6,
            "{}",
            macros[&id].kcal
        );
    }

    #[test]
    fn prune_guard_spares_source_backed_and_real_recipes() {
        use crate::domain::Nutrition;
        // A how-to page: one ingredient → not cookable, no source → prunable.
        let mut guide = Recipe::new("How to Roast Peppers");
        guide.ingredients = vec![normalize_line("2 red bell peppers")];
        assert!(prunable(&guide));
        // Same page, but it publishes nutrition → spared.
        guide.servings = Some(2.0);
        guide.meta.nutrition = Some(Nutrition {
            kcal: Some(50.0),
            protein_g: Some(2.0),
            fat_g: Some(0.0),
            carbs_g: Some(10.0),
        });
        assert!(
            !prunable(&guide),
            "a recipe with usable source nutrition must never be pruned"
        );
        // A normal recipe is never prunable.
        let mut chili = Recipe::new("Chili");
        chili.ingredients = ["2 cans beans", "1 lb beef", "1 onion"]
            .iter()
            .map(|l| normalize_line(l))
            .collect();
        assert!(!prunable(&chili));
    }

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

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use crate::domain::RecipeId;
    use std::collections::HashMap;

    /// Offline fetcher backed by a URL->HTML map; unknown URLs 404.
    struct MapFetcher {
        pages: HashMap<String, String>,
    }
    impl HtmlFetcher for MapFetcher {
        fn fetch(&self, url: &str) -> Result<String> {
            self.pages
                .get(url)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("404: {url}"))
        }
    }

    fn url_recipe(id: &str, title: &str, url: &str) -> Recipe {
        let mut r = Recipe::new(title);
        r.id = RecipeId::from(id);
        r.source = RecipeSource::Url { url: url.into() };
        r.ingredients = vec![normalize_line("1 cup flour")];
        r
    }

    fn html_with_category(name: &str, category: &str) -> String {
        format!(
            r#"<html><head><script type="application/ld+json">
            {{"@context":"https://schema.org","@type":"Recipe","name":"{name}",
             "recipeCategory":"{category}","recipeIngredient":["1 cup flour","2 eggs"]}}
            </script></head><body></body></html>"#
        )
    }

    fn url_targets(recipes: &[Recipe]) -> Vec<(&Recipe, String)> {
        recipes
            .iter()
            .filter_map(|r| match &r.source {
                RecipeSource::Url { url } => Some((r, url.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn refresh_backfills_category_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        store
            .save_recipe(&url_recipe("id1", "Old Title", "https://ex.com/a"))
            .unwrap();

        let mut pages = HashMap::new();
        pages.insert(
            "https://ex.com/a".to_string(),
            html_with_category("Fresh Title", "Sauce"),
        );
        let fetcher = MapFetcher { pages };

        let recipes = store.list_recipes(None).unwrap();
        let report = refresh_recipes(&store, &fetcher, &url_targets(&recipes), 4, false).unwrap();
        assert_eq!(report.updated, 1);
        assert_eq!(report.with_category, 1);

        let updated = store.get_recipe("id1").unwrap().unwrap();
        assert_eq!(updated.meta.category.as_deref(), Some("Sauce"));
        assert_eq!(updated.id.as_str(), "id1"); // id preserved
        assert_eq!(updated.title, "Fresh Title"); // content re-parsed
                                                  // Original source URL kept.
        assert!(matches!(updated.source, RecipeSource::Url { url } if url == "https://ex.com/a"));
    }

    #[test]
    fn refresh_leaves_row_intact_on_fetch_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        store
            .save_recipe(&url_recipe("id1", "Keep Me", "https://ex.com/missing"))
            .unwrap();

        let fetcher = MapFetcher {
            pages: HashMap::new(), // every fetch 404s
        };
        let recipes = store.list_recipes(None).unwrap();
        let report = refresh_recipes(&store, &fetcher, &url_targets(&recipes), 4, false).unwrap();
        assert_eq!(report.updated, 0);
        assert_eq!(report.fetch_failed, 1);

        let unchanged = store.get_recipe("id1").unwrap().unwrap();
        assert_eq!(unchanged.title, "Keep Me"); // untouched
    }

    #[test]
    fn non_url_recipes_are_not_refresh_targets() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let mut manual = Recipe::new("Grandma's Stew");
        manual.id = RecipeId::from("m1");
        manual.ingredients = vec![normalize_line("1 cup beans")];
        // source defaults to Manual
        store.save_recipe(&manual).unwrap();

        let recipes = store.list_recipes(None).unwrap();
        assert!(
            url_targets(&recipes).is_empty(),
            "manual recipe has no URL to refresh"
        );
    }
}
