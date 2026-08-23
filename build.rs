fn main() {
    // 复制资源到 OUT_DIR 并让链接器嵌入 rc 脚本
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/uac-cursor.cur");
    embed_resource::compile("assets/app.rc", embed_resource::NONE);
}
