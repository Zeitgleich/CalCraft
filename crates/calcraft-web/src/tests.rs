use super::*;

#[test]
fn page_links_source_without_inline_code() {
    let page = page();
    assert!(page.contains(calcraft_core::SOURCE_URL));
    assert!(!page.contains("style="));
    assert!(!page.contains("<style"));
    // Scripts only by reference.
    for line in page.lines().filter(|l| l.contains("<script")) {
        assert!(line.contains(" src=\"/assets/"), "{line}");
    }
}

#[test]
fn assets_are_complete_and_hashed() {
    let assets = assets();
    if !ASSETS_EMBEDDED {
        assert!(assets.is_empty());
        return;
    }
    for ext in ["js", "wasm", "css", "svg", "ico", "png", "webmanifest"] {
        assert!(assets.iter().any(|a| a.name.ends_with(ext)), "{ext}");
    }
    for asset in assets {
        let (stem, _) = asset.name.rsplit_once('.').unwrap_or_default();
        let (_, hash) = stem.rsplit_once('-').unwrap_or_default();
        assert_eq!(hash.len(), 16, "{}", asset.name);
        assert!(!asset.body.is_empty());
    }
}
