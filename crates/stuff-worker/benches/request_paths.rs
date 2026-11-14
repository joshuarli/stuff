#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use rustybench::{black_box, AllocProfiler, Bencher};
use deser_value::Value;
use stuff_worker::api;

#[global_allocator]
static ALLOC: AllocProfiler = AllocProfiler::system();

const ROUTES: &str = r#"[{"host":"crates.17166969.xyz","bucket_binding":"MYCRATES","key_prefix":""},{"host":"npm.17166969.xyz","bucket_binding":"MYNPM","key_prefix":""},{"host":"pypi.17166969.xyz","bucket_binding":"PYPI","key_prefix":""},{"host":"oci.17166969.xyz","bucket_binding":"OCI","key_prefix":""}]"#;
const REGISTRIES: [&str; 4] = ["mycrates", "mynpm", "mypypi", "oci"];
const HEAVY_COPIES: usize = 10;
const CRATES_GENERATION: &str = "9ed854790d77710a3a17cdaf";
const INDEX_ETAG: &str = "\"055223bf1bbd39d951d8337e4a50df85\"";

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/catalogs")
        .join(format!("{name}.json"));
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn production_catalogs() -> &'static [String] {
    static CATALOGS: OnceLock<Vec<String>> = OnceLock::new();
    CATALOGS.get_or_init(|| REGISTRIES.iter().map(|name| fixture(name)).collect())
}

fn with_packages(catalog: &str, select: impl FnOnce(&[Value]) -> Vec<Value>) -> String {
    let mut value: Value = deser_json::from_str(catalog).expect("catalog parses");
    let packages = value
        .as_map_mut()
        .and_then(|map| map.get_mut("packages"))
        .and_then(|packages| packages.as_seq_mut())
        .expect("catalog has a packages array");
    let selected = select(packages);
    packages.clear();
    packages.extend(selected);
    deser_json::to_string(&value).expect("catalog serializes")
}

fn renamed(package: &Value, suffix: &str) -> Value {
    let mut copy = package.clone();
    let map = copy.as_map_mut().expect("package is an object");
    let name = map.get("name").and_then(|name| name.as_str()).expect("package has a name");
    let name = format!("{name}{suffix}");
    map.insert("name", name);
    copy
}

fn catalogs_argument(catalogs: &[String]) -> String {
    deser_json::to_string(&catalogs.to_vec()).expect("catalog list serializes")
}

fn small_argument() -> &'static str {
    static ARGUMENT: OnceLock<String> = OnceLock::new();
    ARGUMENT.get_or_init(|| {
        let catalogs: Vec<String> = production_catalogs()
            .iter()
            .map(|catalog| with_packages(catalog, |packages| packages.iter().take(1).cloned().collect()))
            .collect();
        catalogs_argument(&catalogs)
    })
}

fn typical_argument() -> &'static str {
    static ARGUMENT: OnceLock<String> = OnceLock::new();
    ARGUMENT.get_or_init(|| catalogs_argument(production_catalogs()))
}

fn heavy_argument() -> &'static str {
    static ARGUMENT: OnceLock<String> = OnceLock::new();
    ARGUMENT.get_or_init(|| {
        catalogs_argument(
            &production_catalogs()
                .iter()
                .map(|catalog| {
                    with_packages(catalog, |packages| {
                        (0..HEAVY_COPIES)
                            .flat_map(|copy| packages.iter().map(move |package| renamed(package, &format!("-copy{copy}"))))
                            .collect()
                    })
                })
                .collect::<Vec<_>>(),
        )
    })
}

fn render_page(argument: &str) -> String {
    let rendered = api::render_unified_index_json(argument, ROUTES, "https://stuff.17166969.xyz");
    assert!(rendered.starts_with("{\"kind\":\"html\""), "unified render failed: {}", &rendered[..rendered.len().min(200)]);
    rendered
}

#[rustybench::bench]
fn unified_page_small(bencher: Bencher) {
    let argument = small_argument();
    bencher.bench_local(|| render_page(black_box(argument)));
}

#[rustybench::bench]
fn unified_page_production(bencher: Bencher) {
    let argument = typical_argument();
    bencher.bench_local(|| render_page(black_box(argument)));
}

#[rustybench::bench]
fn unified_page_heavy(bencher: Bencher) {
    let argument = heavy_argument();
    bencher.bench_local(|| render_page(black_box(argument)));
}

#[rustybench::bench]
fn unified_llms_txt_production(bencher: Bencher) {
    let argument = typical_argument();
    bencher.bench_local(|| {
        let rendered =
            api::render_unified_llms_txt_json(black_box(argument), ROUTES, "https://stuff.17166969.xyz");
        assert!(rendered.starts_with("{\"kind\":\"markdown\""));
        rendered
    });
}

fn cargo_index_paths() -> &'static [String] {
    static PATHS: OnceLock<Vec<String>> = OnceLock::new();
    PATHS.get_or_init(|| {
        let catalog: Value =
            deser_json::from_str(&production_catalogs()[0]).expect("cargo catalog parses");
        let packages = catalog.get("packages").and_then(|p| p.as_seq()).expect("packages array");
        packages
            .iter()
            .map(|package| {
                let name = package.get("name").and_then(|n| n.as_str()).expect("name").to_ascii_lowercase();
                let tree = match package.get("source").and_then(|s| s.as_str()) {
                    Some("upstream") => "crates-io",
                    _ => "registry",
                };
                let shard = stuff_cargo_shard::shard_for_name(&name).expect("ascii crate name");
                format!("/current/{tree}/{shard}")
            })
            .collect()
    })
}

fn revalidate(path: &str) -> String {
    let mapped = api::map_asset_json(ROUTES, "crates.17166969.xyz", path);
    let rel_path = json_field(&mapped, "rel_path");
    let resolved = api::resolve_pointer_json("current", CRATES_GENERATION, &rel_path);
    let key = json_field(&resolved, "key");
    let conditional = format!("{{\"if_none_match\":{},\"if_match\":null,\"range\":null}}", deser_json::to_string(&INDEX_ETAG).expect("etag"));
    let plan = api::plan_object_response_json("pointer", "GET", &key, &conditional, 420, INDEX_ETAG);
    assert!(plan.contains("\"status\":304"), "{plan}");
    plan
}

fn json_field(document: &str, field: &str) -> String {
    let value: Value = deser_json::from_str(document).expect("api returns JSON");
    value
        .get(field)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{field} missing from {document}"))
        .to_string()
}

#[rustybench::bench]
fn cargo_index_revalidation_burst(bencher: Bencher) {
    let paths = cargo_index_paths();
    bencher.bench_local(|| {
        for path in paths {
            black_box(revalidate(black_box(path)));
        }
    });
}

fn main() {
    rustybench::main();
}
