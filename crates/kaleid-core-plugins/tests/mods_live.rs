//! 联网冒烟测试（默认 `#[ignore]`）。
//!
//! 模组下载只依赖 Modrinth / CurseForge（经 MCIM 镜像）的 JSON 字段名。这些
//! 字段一旦改名，单测里的手写 fixture 发现不了，所以留一条真正发请求的用例：
//!
//! ```text
//! cargo test -p kaleid-core-plugins --test mods_live -- --ignored --nocapture
//! ```
//!
//! 注意它需要外网，CI 里不会跑（`#[ignore]`），本地排查「列表空白 / 字段没了」
//! 时再手动执行。

use std::sync::Arc;

use kaleid_api::{ModDetailQuery, ModSearchQuery, ModSourceFilter};
use kaleid_core_plugins::mods::ModService;
use kaleid_core_plugins::{ModRankingQuery, RootRegistry, Settings, VersionStore};

#[test]
#[ignore = "需要联网；手动 cargo test -- --ignored 执行"]
fn ranking_search_and_detail_parse_live_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let settings = Arc::new(Settings::open(dir.path()));
    let roots = Arc::new(RootRegistry::empty_at(dir.path()).unwrap());
    let versions = Arc::new(VersionStore::new());
    let service = ModService::new(settings, roots, versions, None, dir.path());

    // 排行榜：分面式「热门」，只用 Modrinth，按下载量降序。
    let ranking = service
        .ranking(ModRankingQuery {
            game_version: Some("1.21.4".into()),
            loader: Some("fabric".into()),
            force: true,
            ..Default::default()
        })
        .expect("ranking 请求失败");
    assert!(
        ranking.hits.len() >= 10,
        "排行榜命中太少：{}",
        ranking.hits.len()
    );
    // 分面必须真的生效：fabric + 1.21.4 的页里不该出现别的加载器。
    assert!(
        ranking
            .hits
            .iter()
            .all(|hit| hit.source.as_str() == "modrinth"),
        "热门只应包含 Modrinth 条目"
    );
    for pair in ranking.hits.windows(2) {
        assert!(
            pair[0].downloads >= pair[1].downloads,
            "排行榜没有按下载量降序：{} < {}",
            pair[0].downloads,
            pair[1].downloads
        );
    }
    let first = &ranking.hits[0];
    assert!(!first.name.is_empty());
    assert!(!first.loaders_label.is_empty());
    assert!(!first.page_url.is_empty());

    // 搜索：名字能查到东西，类型筛选能收到 facet。
    let search = service
        .search(ModSearchQuery {
            query: "sodium".into(),
            source: ModSourceFilter::All,
            force: true,
            ..Default::default()
        })
        .expect("search 请求失败");
    assert!(!search.hits.is_empty(), "搜 sodium 没有结果");
    assert!(search
        .hits
        .iter()
        .any(|hit| hit.name.to_lowercase().contains("sodium") || hit.slug.contains("sodium")));

    // 详情：分组、文件、直链都要能装配出来。
    let target = search.hits.first().expect("no hit");
    let detail = service
        .detail(ModDetailQuery {
            source: target.source,
            project_id: target.id.clone(),
            root_id: None,
            version_id: None,
            force: true,
        })
        .expect("detail 请求失败");
    assert!(!detail.groups.is_empty(), "详情没有版本分组");
    let files: Vec<_> = detail.groups.iter().flat_map(|g| g.files.iter()).collect();
    assert!(!files.is_empty(), "详情没有任何文件");
    assert!(
        files.iter().any(|file| !file.official_url.is_empty()),
        "文件都没有直链"
    );

    println!(
        "ranking={} search={} groups={} files={} warnings={:?}",
        ranking.hits.len(),
        search.hits.len(),
        detail.groups.len(),
        files.len(),
        detail.warnings
    );
    println!("first file = {}", files[0].file_name);
    println!(
        "first group = {} {}",
        detail.groups[0].loader_label, detail.groups[0].game_version
    );

    // CurseForge 分支：官方接口没有 API Key，走 MCIM 镜像；顺便验证前置解析。
    let cf = service
        .search(ModSearchQuery {
            query: "jei".into(),
            source: ModSourceFilter::CurseForge,
            force: true,
            ..Default::default()
        })
        .expect("CurseForge 搜索失败");
    assert!(!cf.hits.is_empty(), "CurseForge 搜 jei 没有结果");
    assert!(cf
        .hits
        .iter()
        .all(|hit| hit.source.as_str() == "curseforge"));
    // 回归点：只给 `searchFilter` 时 CF 会按 featured 排序返回一堆不相干模组，
    // 所以请求里必须始终带 `sortField=6`。前五条里至少要有一条真的叫 JEI。
    assert!(
        cf.hits
            .iter()
            .take(5)
            .any(|hit| format!("{}{}", hit.name, hit.slug)
                .to_lowercase()
                .contains("jei")),
        "CurseForge 搜索结果不相关：{:?}",
        cf.hits
            .iter()
            .take(5)
            .map(|hit| &hit.name)
            .collect::<Vec<_>>()
    );
    let cf_hit = cf.hits.first().unwrap();
    let cf_detail = service
        .detail(ModDetailQuery {
            source: cf_hit.source,
            project_id: cf_hit.id.clone(),
            root_id: None,
            version_id: None,
            force: true,
        })
        .expect("CurseForge 详情失败");
    assert!(!cf_detail.groups.is_empty(), "CurseForge 详情没有分组");
    let dep_total: usize = cf_detail
        .groups
        .iter()
        .map(|group| group.dependencies.len())
        .sum();
    println!(
        "cf: {} · {} 组 · 前置条目 {} · warnings={:?}",
        cf_detail.project.name,
        cf_detail.groups.len(),
        dep_total,
        cf_detail.warnings
    );
}
