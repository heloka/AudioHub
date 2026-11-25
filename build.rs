fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=icon.ico");
        println!("cargo:rerun-if-changed=audiohub.rc");
        embed_resource::compile("audiohub.rc", embed_resource::NONE)
            .manifest_required()
            .expect("编译 AudioHub 图标失败");
    }
}
