fn main() {
    slint_build::compile("ui/notch.slint").expect("ui/notch.slint compiles");

    // The exe's icon and the details Explorer shows for it. The version and the file
    // description, which Task Manager shows, come from Cargo.toml.
    println!("cargo::rerun-if-changed=assets/notchling.ico");
    winresource::WindowsResource::new()
        .set_icon("assets/notchling.ico")
        .set("ProductName", "notchling")
        .set("LegalCopyright", "Copyright (c) 2026 John Ansley Rocel")
        .compile()
        .expect("the icon and file details compile into the exe");
}
