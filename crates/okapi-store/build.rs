fn main() {
    // sqlx::migrate! 编译期嵌入迁移目录：新增迁移文件必须触发本 crate 重编，
    // 否则增量构建下运行时 run_migrations 应用不到新文件（本地踩坑实录）。
    println!("cargo:rerun-if-changed=migrations");
}
