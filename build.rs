fn main() {
    // Embed the app icon in the exe.
    embed_resource::compile("assets/stash.rc", embed_resource::NONE).manifest_optional().unwrap();
}
