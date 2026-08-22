//! `Optive custom` 子命令。

use std::path::PathBuf;

use crate::cli::manifest::find_project;
use optive::custom::{
    self, build_active_from_ids, custom_dir, list_installed_ids, load_pack_dir, load_pack_staging,
    parse_use_list, read_global_use, read_project_use, set_active_pack, write_global_use,
    write_project_use, CliMsg, Diag, GLOBAL_CONFIG_FILE, PROJECT_CUSTOM_FILE,
};

fn t(msg: CliMsg) -> String {
    custom::active_pack().render_diag(&Diag::Cli(msg))
}

pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        print_custom_help();
        return Ok(());
    }
    match args[0].as_str() {
        "show" => cmd_show(),
        "all" => cmd_all(),
        "use" => cmd_use(&args[1..])?,
        "add" => cmd_add(&args[1..])?,
        "-h" | "--help" | "help" => print_custom_help(),
        other => return Err(format!("unknown custom subcommand `{other}`").into()),
    }
    Ok(())
}

fn print_custom_help() {
    println!("Optive custom — customization packs (messages + layout)");
    println!();
    println!("  Optive custom show                 Show active pack chain");
    println!("  Optive custom all                  List installed packs");
    println!("  Optive custom use <a,b>            Set project Custom.toml");
    println!("  Optive custom use <a,b> --global   Set global Config.toml");
    println!("  Optive custom add <git-url>        Install pack into $OPTIVE_HOME/custom/");
}

fn cmd_show() {
    let active = custom::active_pack();
    println!("Active: {}", active.chain_display());
    if let Ok(proj) = find_project(None) {
        let p = proj.root.join(PROJECT_CUSTOM_FILE);
        if p.is_file() {
            println!("Custom.toml: {}", p.display());
            if let Ok(ids) = read_project_use(&p) {
                println!("  use = {ids:?}");
            }
        } else {
            println!("Custom.toml: (none)");
        }
    } else {
        println!("Custom.toml: (not in a project)");
    }
    let g = custom_dir().parent().map_or_else(
        || PathBuf::from(GLOBAL_CONFIG_FILE),
        |p| p.join(GLOBAL_CONFIG_FILE),
    );
    println!("Global: {}", g.display());
    if let Ok(ids) = read_global_use() {
        if !ids.is_empty() {
            println!("  use = {ids:?}");
        }
    }
    if let Ok(s) = std::env::var("OPTIVE_CUSTOM") {
        if !s.trim().is_empty() {
            println!("OPTIVE_CUSTOM: {s}");
        }
    }
}

fn cmd_all() {
    let active = custom::active_pack();
    let active_set: std::collections::HashSet<_> = active.chain.iter().cloned().collect();
    println!(
        "en-US (built-in){}",
        if active_set.contains("en-US") {
            "    *"
        } else {
            ""
        }
    );
    for id in list_installed_ids(&custom_dir()) {
        let mark = if active_set.contains(&id) {
            "    *"
        } else {
            ""
        };
        let desc = load_pack_dir(&custom_dir().join(&id))
            .ok()
            .map(|p| p.description)
            .unwrap_or_default();
        if desc.is_empty() {
            println!("{id}{mark}");
        } else {
            println!("{id}{mark}  — {desc}");
        }
    }
}

fn cmd_use(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut global = false;
    let mut ids_raw = None;
    for a in args {
        if a == "--global" {
            global = true;
        } else if ids_raw.is_none() {
            ids_raw = Some(a.as_str());
        } else {
            return Err("usage: Optive custom use <a,b> [--global]".into());
        }
    }
    let Some(raw) = ids_raw else {
        return Err("usage: Optive custom use <a,b> [--global]".into());
    };
    let ids = parse_use_list(raw);
    for id in &ids {
        if id == "en-US" {
            continue;
        }
        let dir = custom_dir().join(id);
        load_pack_dir(&dir).map_err(|e| format!("pack `{id}`: {e}"))?;
    }

    print!("{} ", t(CliMsg::CustomChanging));
    if global {
        write_global_use(&ids)?;
    } else {
        let proj = find_project(None).map_err(|_| {
            "not in an Optive project; use --global or run from a project with Optive.toml"
                .to_string()
        })?;
        write_project_use(&proj.root.join(PROJECT_CUSTOM_FILE), &ids)?;
    }
    let active = build_active_from_ids(&ids)?;
    set_active_pack(active);
    println!("{}", t(CliMsg::CustomDone));
    println!(
        "{} {}",
        t(CliMsg::CustomNow),
        custom::active_pack().chain_display()
    );
    Ok(())
}

fn cmd_add(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let url = args.first().ok_or("usage: Optive custom add <git-url>")?;
    std::fs::create_dir_all(custom_dir())?;
    let tmp = custom_dir().join(".tmp-add");
    let _ = std::fs::remove_dir_all(&tmp);
    crate::cli::git_ops::clone_into(url, &tmp)?;
    // 临时目录名是 `.tmp-add`，不能要求与 id 一致；装到 `custom/<id>/` 后再用 load_pack_dir 校验。
    let pack = load_pack_staging(&tmp).map_err(|e| format!("invalid custom pack: {e}"))?;
    let dest = custom_dir().join(&pack.id);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::rename(&tmp, &dest)?;
    println!("{}", t(CliMsg::CustomAdded));
    println!("  ID: {}", pack.id);
    println!("  Description: {}", pack.description);
    Ok(())
}
