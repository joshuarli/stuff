use std::path::Path;
use std::process::Command;

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

fn stuff(args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_stuff")).args(args).output().unwrap();
    Output {
        code: output.status.code().unwrap(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

fn ok(args: &[&str]) -> String {
    let output = stuff(args);
    assert_eq!(output.code, 0, "stuff {args:?}: {}", output.stderr);
    output.stdout
}

fn write_catalog(dir: &Path, generation: &str, packages: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("catalog.json"),
        format!(r#"{{"schema_version":1,"registry":"npm","generation":"{generation}","packages":[{packages}]}}"#),
    )
    .unwrap();
}

fn package(name: &str, latest: &str, versions: &[(&str, bool)]) -> String {
    let versions: Vec<String> = versions
        .iter()
        .map(|(version, yanked)| format!(r#"{{"version":"{version}","yanked":{yanked}}}"#))
        .collect();
    format!(r#"{{"name":"{name}","source":null,"latest":"{latest}","versions":[{}]}}"#, versions.join(","))
}

#[test]
fn local_catalogs_list_search_diff_and_render() {
    let work = tempfile::tempdir().unwrap();
    let (before, after) = (work.path().join("before"), work.path().join("after"));
    write_catalog(
        &before,
        "g1",
        &[package("alpha", "1.0.0", &[("1.0.0", false)]), package("gone", "1.0.0", &[("1.0.0", false)])].join(","),
    );
    write_catalog(
        &after,
        "g2",
        &[
            package("alpha", "1.1.0", &[("1.0.0", true), ("1.1.0", false)]),
            package("beta", "2.0.0", &[("2.0.0", false)]),
        ]
        .join(","),
    );
    let (before_arg, after_arg) = (before.to_str().unwrap(), after.to_str().unwrap());

    assert_eq!(
        ok(&["ls", "--npm-dir", before_arg]),
        "REGISTRY PACKAGE                  SOURCE    VERSIONS LATEST\n\
         npm      alpha                    -         1        1.0.0\n\
         npm      gone                     -         1        1.0.0\n\
         \n\
         2 packages · 2 versions\n"
    );
    let versions = ok(&["ls", "--npm-dir", after_arg, "--versions"]);
    assert!(versions.contains("1.0.0 (yanked)\n"), "{versions}");
    assert!(versions.contains("1.1.0 (latest)\n"), "{versions}");
    let json = ok(&["ls", "--npm-dir", before_arg, "--json"]);
    assert!(json.starts_with("{\"catalogs\":[{") && json.contains("\"generation\": \"g1\""), "{json}");

    let found = ok(&["search", "ALP", "--npm-dir", after_arg]);
    assert!(found.contains("alpha") && !found.contains("beta"), "{found}");
    assert_eq!(ok(&["search", "zzz", "--npm-dir", after_arg]), "no packages match 'zzz'\n");
    let mixed = work.path().join("mixed");
    write_catalog(&mixed, "g3", &package("Gamma-Tool", "3.0.0", &[("3.0.0", false)]));
    assert!(ok(&["search", "gamma", "--npm-dir", mixed.to_str().unwrap()]).contains("Gamma-Tool"));

    assert_eq!(
        ok(&["diff", before_arg, after_arg]),
        "added package beta\nremoved package gone\nadded alpha 1.1.0\nyanked alpha 1.0.0\nlatest alpha: 1.0.0 -> 1.1.0\n"
    );
    assert_eq!(
        ok(&["diff", before_arg, after_arg, "--json"]),
        "{\"added_packages\":[{\"name\":\"beta\",\"source\":null}],\"removed_packages\":[{\"name\":\"gone\",\"source\":null}],\"changed\":[{\"name\":\"alpha\",\"source\":null,\"added_versions\":[\"1.1.0\"],\"removed_versions\":[],\"yanked_now\":[\"1.0.0\"],\"unyanked_now\":[],\"latest_before\":\"1.0.0\",\"latest_after\":\"1.1.0\"}]}\n"
    );
    assert_eq!(ok(&["diff", before_arg, before_arg]), "no changes (g1 -> g1)\n");

    let page = work.path().join("index.html");
    let llms = work.path().join("llms.txt");
    assert_eq!(
        ok(&["render", "--npm-dir", after_arg, "--output", page.to_str().unwrap()]),
        format!("rendered 1 catalogs to {}\n", page.display())
    );
    ok(&["render", "--npm-dir", after_arg, "--output", llms.to_str().unwrap()]);
    let html = std::fs::read_to_string(&page).unwrap();
    assert!(html.contains("<html") && html.contains("alpha") && html.contains("beta"), "{html}");
    let text = std::fs::read_to_string(&llms).unwrap();
    assert!(text.starts_with("# stuff registries") && text.contains("`beta`"), "{text}");
}

#[test]
fn failures_map_to_the_documented_exit_codes() {
    let work = tempfile::tempdir().unwrap();
    let broken = work.path().join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("catalog.json"), "{").unwrap();
    let healthy = work.path().join("healthy");
    write_catalog(&healthy, "g1", &package("alpha", "1.0.0", &[("1.0.0", false)]));
    let (broken_arg, healthy_arg) = (broken.to_str().unwrap(), healthy.to_str().unwrap());
    let missing = work.path().join("missing");

    for args in [
        vec!["bogus"],
        vec!["ls", "--registry", "bogus"],
        vec!["ls", "extra"],
        vec!["search"],
        vec!["diff", healthy_arg],
        vec!["render", "--npm-dir", healthy_arg],
        vec!["doctor", "--remote"],
        vec!["ls", "--npm-dir", missing.to_str().unwrap()],
    ] {
        let output = stuff(&args);
        assert_eq!(output.code, 2, "{args:?}: {}", output.stderr);
        assert!(output.stderr.starts_with("stuff: "), "{}", output.stderr);
    }

    let corrupt = stuff(&["ls", "--npm-dir", broken_arg]);
    assert_eq!(corrupt.code, 3, "{}", corrupt.stderr);
    assert!(corrupt.stderr.starts_with("stuff: corrupt: "), "{}", corrupt.stderr);

    let failed = stuff(&["doctor", "--npm-dir", broken_arg]);
    assert_eq!(failed.code, 1, "{}", failed.stderr);
    assert!(failed.stdout.contains("FAIL: catalog load"), "{}", failed.stdout);
    let incomplete = stuff(&["doctor", "--npm-dir", healthy_arg]);
    assert_eq!(incomplete.code, 1);
    assert!(incomplete.stdout.contains("ok: npm catalog"), "{}", incomplete.stdout);
}
