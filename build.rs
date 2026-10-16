// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Generate configuration and bundled GTK resources under Cargo's OUT_DIR.
//! Neither Cargo nor Meson needs to write generated files into the source tree.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

const APP_ID: &str = "io.github.Certain1989.Broadcaster";

fn main() {
    println!("cargo:rerun-if-env-changed=BROADCASTER_CONFIG_RS");
    for path in [
        "src/config.rs.in",
        "src/shortcuts-dialog.blp",
        "data/io.github.Certain1989.Broadcaster.metainfo.xml.in",
        "data/resources",
        "data/icons/hicolor/128x128/apps/io.github.Certain1989.Broadcaster.png",
        "po",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }

    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("missing source path"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("missing Cargo OUT_DIR"));

    write_config(&source, &out);
    write_resources(&source, &out);
}

fn write_config(source: &Path, out: &Path) {
    let configured = env::var_os("BROADCASTER_CONFIG_RS").map(PathBuf::from);
    let config = if let Some(path) = configured {
        println!("cargo:rerun-if-changed={}", path.display());
        fs::read_to_string(&path).expect("Meson-generated config.rs is missing")
    } else {
        // Direct Cargo invocation without Meson: provide development defaults.
        let version = format!(
            "{}-devel",
            env::var("CARGO_PKG_VERSION").expect("missing Cargo package version")
        );
        let template =
            fs::read_to_string(source.join("src/config.rs.in")).expect("missing config.rs.in");
        let variables = [
            ("PROJECT_NAME", "broadcaster".to_owned()),
            ("VERSION", version),
            ("PROFILE", "Devel".to_owned()),
            ("APP_ID", format!("{APP_ID}.Devel")),
            ("BASE_APP_ID", APP_ID.to_owned()),
            ("APP_NAME", "Broadcaster".to_owned()),
            (
                "AUTHOR",
                "David Cabot, Televido, Televido-Fritz and Broadcaster contributors".to_owned(),
            ),
            (
                "PROJECT_URL",
                "https://github.com/Certain1989/Broadcaster".to_owned(),
            ),
            (
                "ISSUE_URL",
                "https://github.com/Certain1989/Broadcaster/issues".to_owned(),
            ),
            ("LOCALEDIR", "/usr/local/share/locale".to_owned()),
            ("PKGDATADIR", "/usr/local/share/broadcaster".to_owned()),
        ];
        variables.iter().fold(template, |text, (key, value)| {
            text.replace(&format!("@{key}@"), value)
        })
    };
    fs::write(out.join("config.rs"), config).expect("cannot write generated config.rs");
}

fn write_resources(source: &Path, out: &Path) {
    let resources = out.join("resources");
    fs::create_dir_all(&resources).expect("cannot create resource output directory");

    let mut entries = Vec::new();
    let resource_root = source.join("data/resources");
    copy_tree(
        &resource_root,
        &resource_root,
        &resources,
        Path::new(""),
        &mut entries,
    );

    // The desktop icon remains installed in hicolor for the desktop shell.
    // Also embed the same verified PNG for the in-process About dialog:
    // Flatpak renames the installed launcher icon and the icon theme cache
    // cannot always resolve it at runtime on Wayland.
    let about_icon_path = format!("icons/128x128/apps/{APP_ID}.png");
    let about_icon = resources.join(&about_icon_path);
    fs::create_dir_all(about_icon.parent().expect("icon has no parent"))
        .expect("cannot stage About icon directory");
    fs::copy(
        source.join(format!("data/icons/hicolor/128x128/apps/{APP_ID}.png")),
        &about_icon,
    )
    .expect("cannot stage bundled About icon");
    entries.push(about_icon_path);

    // AdwAboutDialog receives a *named* GTK icon, not a PNG filename.
    // Flatpak renames the launcher icon from BASE_APP_ID to APP_ID.Devel and
    // GTK can retain a stale on-disk base-name reference. Give the dialog a
    // distinct resource-only name to avoid that filesystem icon-cache entry.
    // This is an identical copy, not a new drawing or installed launcher.
    let about_alias_path = format!("icons/128x128/apps/{APP_ID}-about.png");
    fs::copy(&about_icon, resources.join(&about_alias_path))
        .expect("cannot stage uniquely named About icon");
    entries.push(about_alias_path);

    // GTK uses the same resource names as before; only their build location changes.
    let blueprint = source.join("src/shortcuts-dialog.blp");
    run(
        Command::new("blueprint-compiler")
            .arg("batch-compile")
            .arg(out)
            .arg(source.join("src"))
            .arg(&blueprint),
        "compile shortcuts Blueprint",
    );
    fs::copy(
        out.join("shortcuts-dialog.ui"),
        resources.join("shortcuts-dialog.ui"),
    )
    .expect("cannot stage shortcuts-dialog.ui");
    entries.push("shortcuts-dialog.ui".to_owned());

    let metainfo = format!("{APP_ID}.metainfo.xml");
    run(
        Command::new("msgfmt")
            .arg("--xml")
            .arg(format!("-d{}", source.join("po").display()))
            .arg(format!(
                "--template={}",
                source.join(format!("data/{metainfo}.in")).display()
            ))
            .arg(format!(
                "--output-file={}",
                resources.join(&metainfo).display()
            )),
        "translate embedded AppStream metadata",
    );
    entries.push(metainfo);

    entries.sort();
    let mut manifest = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gresources>\n  <gresource prefix=\"/{}\">\n",
        APP_ID.replace('.', "/")
    );
    for entry in entries {
        manifest.push_str(&format!("    <file compressed=\"true\">{entry}</file>\n"));
    }
    manifest.push_str("  </gresource>\n</gresources>\n");
    let manifest_path = out.join("broadcaster.gresource.xml");
    fs::write(&manifest_path, manifest).expect("cannot write resource manifest");

    run(
        Command::new("glib-compile-resources")
            .arg(&manifest_path)
            .arg(format!(
                "--target={}",
                out.join("broadcaster.gresource").display()
            ))
            .arg(format!("--sourcedir={}", resources.display())),
        "compile GResource bundle",
    );
}

fn copy_tree(
    root: &Path,
    source: &Path,
    destination: &Path,
    relative: &Path,
    entries: &mut Vec<String>,
) {
    for entry in fs::read_dir(source).expect("cannot list resource directory") {
        let entry = entry.expect("cannot read resource directory entry");
        let path = entry.path();
        let relative_path = relative.join(entry.file_name());
        let kind = entry.file_type().expect("cannot inspect resource file");
        if kind.is_dir() {
            copy_tree(root, &path, destination, &relative_path, entries);
        } else if kind.is_file() || kind.is_symlink() {
            // Stage in-tree resource symlinks as ordinary files: GResource
            // cannot load the symlink itself from the staging directory.
            if kind.is_symlink() {
                let resolved = fs::canonicalize(&path).expect("invalid resource symlink");
                let root = fs::canonicalize(root).expect("invalid resource root");
                assert!(
                    resolved.starts_with(root) && resolved.is_file(),
                    "resource symlink must target a regular in-tree file: {}",
                    path.display()
                );
            }
            let target = destination.join(&relative_path);
            fs::create_dir_all(target.parent().expect("resource has no parent"))
                .expect("cannot create resource subdirectory");
            fs::copy(&path, &target).expect("cannot stage resource");
            entries.push(relative_path.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn run(command: &mut Command, task: &str) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("{task}: could not start required build tool: {error}"));
    assert!(status.success(), "{task} failed with status {status}");
}
