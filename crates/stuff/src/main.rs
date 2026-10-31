#![forbid(unsafe_code)]

use stuff::cli::{parse_args, print_help, render_table, Command};
use stuff::doctor::{load_catalogs, run_doctor, RegistrySources};

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }
    let parsed = match parse_args(&argv) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("stuff: {e}");
            eprintln!("usage: stuff <ls|search|diff|doctor|render> [options]");
            std::process::exit(2);
        }
    };
    let code = match parsed.command {
        Command::Ls => run_ls(&parsed),
        Command::Search => run_search(&parsed),
        Command::Diff => run_diff(&parsed),
        Command::Doctor => match run_doctor(&parsed) {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(e) => {
                eprintln!("stuff doctor: {e}");
                2
            }
        },
        Command::Render => run_render(&parsed),
    };
    std::process::exit(code);
}

fn run_ls(parsed: &stuff::cli::Parsed) -> i32 {
    let sources = RegistrySources::from_parsed(parsed);
    let catalogs = match load_catalogs(&sources, parsed.registry_filter()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("stuff ls: {e}");
            return 2;
        }
    };
    if parsed.json {
        print!("{{\"catalogs\":[");
        for (i, catalog) in catalogs.iter().enumerate() {
            if i > 0 {
                print!(",");
            }
            print!("{}", stuff_catalog::render_catalog_json(catalog).trim_end());
        }
        println!("]}}");
    } else {
        print!("{}", render_table(&catalogs, parsed.versions));
    }
    0
}

fn run_search(parsed: &stuff::cli::Parsed) -> i32 {
    let sources = RegistrySources::from_parsed(parsed);
    let mut catalogs = match load_catalogs(&sources, parsed.registry_filter()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("stuff search: {e}");
            return 2;
        }
    };
    let query = parsed.positionals[0].to_lowercase();
    for catalog in &mut catalogs {
        catalog.packages.retain(|p| p.name.to_lowercase().contains(&query));
    }
    catalogs.retain(|c| !c.packages.is_empty());
    if parsed.json {
        print!("{{\"catalogs\":[");
        for (i, catalog) in catalogs.iter().enumerate() {
            if i > 0 {
                print!(",");
            }
            print!("{}", stuff_catalog::render_catalog_json(catalog).trim_end());
        }
        println!("]}}");
    } else if catalogs.iter().all(|c| c.packages.is_empty()) {
        println!("no packages match '{}'", parsed.positionals[0]);
    } else {
        print!("{}", render_table(&catalogs, false));
    }
    0
}

fn run_diff(parsed: &stuff::cli::Parsed) -> i32 {
    let before = match stuff::source::load_local_catalog(&parsed.positionals[0]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("stuff diff: {e}");
            return 2;
        }
    };
    let after = match stuff::source::load_local_catalog(&parsed.positionals[1]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("stuff diff: {e}");
            return 2;
        }
    };
    let diff = match stuff_catalog::diff_catalogs(&before, &after) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("stuff diff: {e}");
            return 2;
        }
    };
    if parsed.json {
        print_diff_json(&diff);
    } else if diff.is_empty() {
        println!("no changes ({} -> {})", before.generation, after.generation);
    } else {
        for name in &diff.added_packages {
            println!("added package {}", package_ref_label(name));
        }
        for name in &diff.removed_packages {
            println!("removed package {}", package_ref_label(name));
        }
        for pkg in &diff.changed_packages {
            let label = package_label(&pkg.name, pkg.source.as_deref());
            for v in &pkg.added_versions {
                println!("added {} {}", label, v);
            }
            for v in &pkg.removed_versions {
                println!("removed {} {}", label, v);
            }
            for v in &pkg.yanked_now {
                println!("yanked {} {}", label, v);
            }
            for v in &pkg.unyanked_now {
                println!("unyanked {} {}", label, v);
            }
            if let (Some(a), Some(b)) = (&pkg.latest_before, &pkg.latest_after) {
                println!("latest {}: {a} -> {b}", label);
            }
        }
    }
    0
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn package_label(name: &str, source: Option<&str>) -> String {
    match source {
        Some(source) => format!("{name} ({source})"),
        None => name.to_string(),
    }
}

fn package_ref_label(package: &stuff_catalog::PackageRef) -> String {
    package_label(&package.name, package.source.as_deref())
}

fn escape_json_option(value: Option<&str>) -> String {
    match value {
        Some(value) => escape_json(value),
        None => "null".to_string(),
    }
}

fn print_diff_json(diff: &stuff_catalog::CatalogDiff) {
    let mut out = String::from("{\"added_packages\":[");
    for (i, package) in diff.added_packages.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"name\":{},\"source\":{}}}",
            escape_json(&package.name),
            escape_json_option(package.source.as_deref())
        ));
    }
    out.push_str("],\"removed_packages\":[");
    for (i, package) in diff.removed_packages.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"name\":{},\"source\":{}}}",
            escape_json(&package.name),
            escape_json_option(package.source.as_deref())
        ));
    }
    out.push_str("],\"changed\":[");
    for (i, pkg) in diff.changed_packages.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"name\":{},\"source\":{},\"added_versions\":[",
            escape_json(&pkg.name),
            escape_json_option(pkg.source.as_deref())
        ));
        for (j, v) in pkg.added_versions.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&escape_json(v));
        }
        out.push_str("],\"removed_versions\":[");
        for (j, v) in pkg.removed_versions.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&escape_json(v));
        }
        out.push_str("],\"yanked_now\":[");
        for (j, v) in pkg.yanked_now.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&escape_json(v));
        }
        out.push_str("],\"unyanked_now\":[");
        for (j, v) in pkg.unyanked_now.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&escape_json(v));
        }
        out.push_str("],\"latest_before\":");
        match &pkg.latest_before {
            Some(v) => out.push_str(&escape_json(v)),
            None => out.push_str("null"),
        }
        out.push_str(",\"latest_after\":");
        match &pkg.latest_after {
            Some(v) => out.push_str(&escape_json(v)),
            None => out.push_str("null"),
        }
        out.push('}');
    }
    out.push_str("]}\n");
    print!("{out}");
}

fn run_render(parsed: &stuff::cli::Parsed) -> i32 {
    let output = match parsed.output.as_deref() {
        Some(o) => o,
        None => {
            eprintln!("stuff render: --output is required");
            return 2;
        }
    };
    let sources = RegistrySources::from_parsed(parsed);
    let catalogs = match load_catalogs(&sources, parsed.registry_filter()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("stuff render: {e}");
            return 2;
        }
    };
    let body = if output.ends_with(".txt") || output.ends_with(".md") {
        stuff_catalog::render_unified_llms_txt(&catalogs)
    } else {
        stuff_catalog::render_unified(&catalogs)
    };
    match std::fs::write(output, body.as_bytes()) {
        Ok(()) => {
            println!("rendered {} catalogs to {output}", catalogs.len());
            0
        }
        Err(e) => {
            eprintln!("stuff render: could not write {output}: {e}");
            2
        }
    }
}
