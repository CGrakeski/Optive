#![allow(clippy::unwrap_used, clippy::expect_used)]

use optive::token::KEYWORDS;

#[test]
fn textmate_keywords_cover_token_rs() {
    let tm = include_str!("../tools/syntax/tive.tmLanguage.json");
    for kw in KEYWORDS {
        assert!(
            tm.contains(kw),
            "tools/syntax/tive.tmLanguage.json missing keyword `{kw}`; run python tools/gen-tm-keywords.py"
        );
    }
}
