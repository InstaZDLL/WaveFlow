use super::*;

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

#[test]
fn same_inside_contains_and_apart() {
    let music = p("/home/u/Music");
    assert_eq!(relation(&music, &music), Some(FolderRelation::Same));
    assert_eq!(
        relation(&p("/home/u/Music/Albums"), &music),
        Some(FolderRelation::Inside)
    );
    assert_eq!(
        relation(&music, &p("/home/u/Music/Albums")),
        Some(FolderRelation::Contains)
    );
    assert_eq!(relation(&p("/home/u/Downloads"), &music), None);
}

#[test]
fn a_shared_prefix_of_a_name_is_not_containment() {
    // `Music2` starts with the characters of `Music` but is not under it.
    assert_eq!(relation(&p("/home/u/Music2"), &p("/home/u/Music")), None);
}

#[test]
fn dots_and_a_trailing_separator_change_nothing() {
    let music = p("/home/u/Music");
    assert_eq!(
        relation(&p("/home/u/Music/"), &music),
        Some(FolderRelation::Same)
    );
    assert_eq!(
        relation(&p("/home/u/Downloads/../Music/./Albums"), &music),
        Some(FolderRelation::Inside)
    );
}

#[test]
fn overlap_prefers_same_then_inside_then_contains() {
    let candidate = p("/m/Music/Albums");
    let under = p("/m/Music/Albums/Live");
    let above = p("/m/Music");
    let same = p("/m/Music/Albums");
    let apart = p("/m/Downloads");
    let pick = |folders: &[&PathBuf]| overlap(&candidate, folders.iter().map(|f| f.as_path()));

    assert_eq!(pick(&[&apart]), None);
    assert_eq!(
        pick(&[&under]),
        Some((FolderRelation::Contains, under.clone()))
    );
    assert_eq!(
        pick(&[&under, &above]),
        Some((FolderRelation::Inside, above.clone()))
    );
    assert_eq!(
        pick(&[&under, &above, &same, &apart]),
        Some((FolderRelation::Same, same.clone()))
    );
}

#[test]
fn the_managed_folder_is_indexed_by_its_row_or_the_folder_holding_it() {
    let managed = p("/m/Music/WaveFlow");
    let music = (1, p("/m/Music"));
    let deeper = (5, p("/m/Music/"));
    let own = (2, managed.clone());
    let other = (3, p("/m/Downloads"));
    assert_eq!(
        indexing_folder(&managed, std::slice::from_ref(&other)),
        Ok(None)
    );
    assert_eq!(
        indexing_folder(&managed, &[music.clone(), other]),
        Ok(Some(1))
    );
    assert_eq!(
        indexing_folder(&managed, &[music.clone(), own]),
        Ok(Some(2))
    );
    // A trailing separator is not a deeper folder.
    assert_eq!(indexing_folder(&managed, &[music, deeper]), Ok(Some(1)));
    let under = (4, managed.join("Album"));
    assert_eq!(
        indexing_folder(&managed, std::slice::from_ref(&under)),
        Err((FolderRelation::Contains, under.1))
    );
}

#[cfg(windows)]
#[test]
fn windows_ignores_case_and_separator_style() {
    assert_eq!(
        relation(&p(r"c:\users\u\music\Albums"), &p("C:/Users/U/Music")),
        Some(FolderRelation::Inside)
    );
}
