//! Builds the native macOS shell: the SwiftUI sources under
//! `src/shell/macos/swift` are compiled into a static library and linked
//! into the binary. Other targets need nothing from here.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        swift_shell();
    }
}

fn xcrun(args: &[&str]) -> String {
    let out = Command::new("xcrun")
        .args(args)
        .output()
        .expect("xcrun not found: install the Xcode Command Line Tools");
    assert!(out.status.success(), "xcrun {args:?} failed");
    String::from_utf8(out.stdout)
        .expect("xcrun output")
        .trim()
        .to_owned()
}

fn swift_shell() {
    let root = Path::new("src/shell/macos");
    println!("cargo:rerun-if-changed={}", root.join("swift").display());
    println!("cargo:rerun-if-changed={}", root.join("include").display());
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");

    let min = env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_default();
    let major: u32 = min
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .unwrap_or(0);
    assert!(
        major >= 26,
        "MACOSX_DEPLOYMENT_TARGET is {min:?}; the SwiftUI shell needs 26.0 (set in .cargo/config.toml)"
    );
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => panic!("unsupported macOS architecture {other:?}"),
    };

    let mut sources: Vec<PathBuf> = std::fs::read_dir(root.join("swift"))
        .expect("swift sources")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "swift"))
        .collect();
    sources.sort();

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let lib = out.join("libRawSpeedTestUI.a");
    let sdk = xcrun(&["--show-sdk-path"]);
    let swiftc = xcrun(&["--find", "swiftc"]);
    let release = env::var("PROFILE").as_deref() == Ok("release");

    let mut cmd = Command::new(&swiftc);
    cmd.args(["-parse-as-library", "-emit-library", "-static", "-wmo"])
        .args(["-module-name", "RawSpeedTestUI", "-swift-version", "6"])
        .args(["-target", &format!("{arch}-apple-macos{min}")])
        .args(["-sdk", &sdk])
        .arg("-I")
        .arg(root.join("include"))
        .args(["-warnings-as-errors"])
        .args(if release {
            &["-O"][..]
        } else {
            &["-Onone", "-g"][..]
        })
        .arg("-module-cache-path")
        .arg(out.join("module-cache"))
        .arg("-o")
        .arg(&lib)
        .args(&sources);
    let status = cmd.status().expect("run swiftc");
    assert!(status.success(), "swiftc failed");

    println!("cargo:rustc-link-search=native={}", out.display());
    // Whole archive: protocol conformance records are only reached through
    // metadata, never by symbol, and would otherwise be dropped.
    println!("cargo:rustc-link-lib=static:+whole-archive=RawSpeedTestUI");
    // The Swift runtime and overlays ship with macOS 26; link against the
    // SDK's stubs and the toolchain's static compatibility shims.
    println!("cargo:rustc-link-search=native={sdk}/usr/lib/swift");
    let toolchain = Path::new(&swiftc)
        .parent()
        .and_then(Path::parent)
        .map(|p| p.join("lib/swift/macosx"));
    if let Some(dir) = toolchain {
        println!("cargo:rustc-link-search=native={}", dir.display());
    }
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    // `#available` checks in optimized Swift call compiler-rt's
    // `__isPlatformVersionAtLeast`; rustc links with -nodefaultlibs, so
    // clang's runtime library has to be named.
    let resource = xcrun(&["clang", "-print-resource-dir"]);
    println!("cargo:rustc-link-search=native={resource}/lib/darwin");
    println!("cargo:rustc-link-lib=static=clang_rt.osx");
}
