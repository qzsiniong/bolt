//! 脚本领域类型与磁盘布局：[`script`] 定义 `Script`/`Language`，
//! [`script_store`] 是「一个目录 ↔ 一组脚本」的文件操作（无状态），
//! [`paths`] 是应用数据目录的唯一事实来源。

pub mod paths;
pub mod script;
pub mod script_store;

pub use script::{Language, Script};

#[cfg(test)]
pub(crate) mod test_support {
    //! 测试用的临时目录（与宿主 `test_support::TempDir` 同款）。

    use std::path::PathBuf;

    /// 临时目录，drop 时整棵删掉。目录名带 tag 和进程 id，并行测试不撞车。
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        /// 建一个干净的同名临时目录（先删旧目录，避免上次残留）。
        pub fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("bolt-model-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        /// 把 `content` 写到 `<dir>/<rel>`，父目录不存在就建出来。
        pub fn write(&self, rel: &str, content: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
