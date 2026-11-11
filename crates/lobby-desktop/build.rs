fn main() {
    let windows = std::env::var("TARGET").is_ok_and(|t| t.ends_with("windows-msvc"));
    // A debug build lays out deep element trees (the gallery's Kit page, a Message with a nested
    // list) past Windows' 1 MiB main-thread stack.
    if windows {
        println!("cargo:rustc-link-arg-bin=lobby-gallery=/STACK:8388608");
        println!("cargo:rustc-link-arg-bin=lobby=/STACK:8388608");
    }
    #[cfg(windows)]
    if windows {
        // GPUI loads icon resource 1 for the window class; the tray loads it too.
        let rc = "../../packaging/windows/lobby.rc";
        println!("cargo:rerun-if-changed={rc}");
        println!("cargo:rerun-if-changed=../../packaging/windows/lobby.ico");
        embed_resource::compile_for(rc, ["lobby"], embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
