//! Sets a `backend_<name>` cfg for every service manager this build compiles: the ones native to
//! the target OS, plus any enabled elsewhere through the feature of the same name.

use std::env;

/// Each backend and the target OSes it is native to.
///
/// Cargo.toml lists each backend's dependencies under these same targets; keep the two in sync.
const BACKENDS: &[(&str, &[&str])] = &[
    ("launchd", &["macos"]),
    ("openrc", &["linux"]),
    ("rcd", &["freebsd", "dragonfly", "openbsd", "netbsd"]),
    ("sc", &["windows"]),
    ("systemd", &["linux"]),
    ("winsw", &["windows"]),
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("cargo sets CARGO_CFG_TARGET_OS");
    for (name, native_oses) in BACKENDS {
        println!("cargo:rustc-check-cfg=cfg(backend_{name})");

        let feature = format!("CARGO_FEATURE_{}", name.to_uppercase());
        if native_oses.contains(&target_os.as_str()) || env::var_os(feature).is_some() {
            println!("cargo:rustc-cfg=backend_{name}");
        }
    }
}
