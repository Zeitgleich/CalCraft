use super::*;

fn meta(size: i64) -> ItemMeta {
    ItemMeta {
        name: "x.ics".into(),
        etag: "\"e\"".into(),
        size,
    }
}

#[test]
fn budget_counts_results_and_bytes() {
    let mut budget = Budget::new(2, false);
    assert!(budget.take(Some(&meta(1))));
    assert!(budget.take(None));
    assert!(!budget.take(Some(&meta(1))));

    let mut bytes = Budget::new(100, true);
    for _ in 0..32 {
        assert!(bytes.take(Some(&meta(1024 * 1024))));
    }
    assert!(!bytes.take(Some(&meta(1))));
    // Without object data, sizes do not count.
    let mut no_data = Budget::new(100, false);
    assert!((0..50).all(|_| no_data.take(Some(&meta(1024 * 1024)))));
}
