use super::*;

fn sorted(names: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    out.sort_by(|a, b| compare(a, b));
    out
}

#[test]
fn accented_initials_sort_with_their_letter() {
    assert_eq!(
        sorted(&["Zoé", "Émilie", "eric", "Alain", "Ödland", "Oasis"]),
        ["Alain", "Émilie", "eric", "Oasis", "Ödland", "Zoé"]
    );
}

/// Letters `normalize_name`'s table does not list still fold: through
/// NFD for the decomposable ones, through `fold_stroke` for the rest.
#[test]
fn accents_outside_the_fold_table_sort_with_their_letter() {
    assert_eq!(
        sorted(&["Zeta", "Řeka", "Rosa", "Ştefan", "Łódź", "Lima", "Sara"]),
        ["Lima", "Łódź", "Řeka", "Rosa", "Sara", "Ştefan", "Zeta"]
    );
}

#[test]
fn ties_resolve_to_a_total_order() {
    assert_eq!(compare("Eric", "Éric"), Ordering::Less);
    assert_eq!(compare("eric", "Eric"), Ordering::Greater);
    assert_eq!(compare("Éric", "Éric"), Ordering::Equal);
    // Decomposed and precomposed spellings differ only past the fold.
    assert_ne!(compare("E\u{301}ric", "Éric"), Ordering::Equal);
    assert_eq!(sorted(&["E\u{301}ric", "Erin"]), ["E\u{301}ric", "Erin"]);
}

#[test]
fn non_latin_scripts_keep_their_code_point_order() {
    assert_eq!(
        sorted(&["東京", "Abba", "Ябеда"]),
        ["Abba", "Ябеда", "東京"]
    );
}

fn sorted_names(names: &[&str], ignore_articles: bool) -> Vec<String> {
    let mut out: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    out.sort_by(|a, b| compare_names(a, b, ignore_articles));
    out
}

#[test]
fn a_leading_article_is_set_aside() {
    assert_eq!(
        sorted_names(
            &[
                "The Beatles",
                "Air",
                "Les Rita Mitsouko",
                "L'Impératrice",
                "Blur",
                "Muse"
            ],
            true
        ),
        [
            "Air",
            "The Beatles",
            "Blur",
            "L'Impératrice",
            "Muse",
            "Les Rita Mitsouko"
        ]
    );
}

#[test]
fn off_it_is_the_plain_fold() {
    assert_eq!(
        sorted_names(&["The Beatles", "Blur", "Air"], false),
        ["Air", "Blur", "The Beatles"]
    );
}

#[test]
fn what_is_not_an_article_stays() {
    // A word that only starts like one, a name that is only one, a
    // hyphen rather than a space, and the curly apostrophe.
    assert_eq!(strip_article("Theory of a Deadman"), "Theory of a Deadman");
    assert_eq!(strip_article("The"), "The");
    assert_eq!(strip_article("The The"), "The");
    assert_eq!(strip_article("A-ha"), "A-ha");
    assert_eq!(strip_article("Lana Del Rey"), "Lana Del Rey");
    assert_eq!(strip_article("L\u{2019}Impératrice"), "Impératrice");
    assert_eq!(strip_article("  the  Kills"), "Kills");
    assert_eq!(strip_article("L'"), "L'");
}

/// What the listings actually sort: the scanner's canonical keys,
/// lowercased with punctuation turned into spaces.
#[test]
fn canonical_keys_lose_their_article_too() {
    assert_eq!(strip_article("l impératrice"), "impératrice");
    assert_eq!(strip_article("the beatles"), "beatles");
    assert_eq!(strip_article("lana del rey"), "lana del rey");
    assert_eq!(strip_article("het goede doel"), "goede doel");
    assert_eq!(strip_article("de la soul"), "de la soul");
    assert_eq!(
        sorted_names(&["the beatles", "l impératrice", "blur", "muse"], true),
        ["the beatles", "blur", "l impératrice", "muse"]
    );
}

#[test]
fn the_full_name_breaks_the_tie() {
    assert_eq!(compare_names("Kills", "The Kills", true), Ordering::Less);
    assert_eq!(
        compare_names("The Kills", "The Kills", true),
        Ordering::Equal
    );
}

#[tokio::test]
async fn sqlite_sorts_with_the_registered_collation() {
    use sqlx::sqlite::SqlitePoolOptions;
    use std::str::FromStr;

    let options = register(SqliteConnectOptions::from_str(":memory:").unwrap());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM (SELECT 'Zoé' AS name UNION ALL SELECT 'Émilie'
                               UNION ALL SELECT 'Alain')
              ORDER BY name COLLATE FOLD",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(names, ["Alain", "Émilie", "Zoé"]);

    // NAME is registered beside it; the flag's default sets the article
    // aside. The test does not touch the flag: other tests share it.
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM (SELECT 'The Beatles' AS name UNION ALL SELECT 'Blur'
                               UNION ALL SELECT 'Air')
              ORDER BY name COLLATE NAME",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(names, ["Air", "The Beatles", "Blur"]);
}
