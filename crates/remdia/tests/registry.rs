use remdia::{Engine, Request};
use serde_json::json;
struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "remdia-registry-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn manifest(&self, file: &str, library: &str, os: &str) {
        std::fs::write(self.0.join(file),serde_json::to_vec(&json!({"id":"test","abi_major":2,"os":os,"arch":std::env::consts::ARCH,"library":library,"capabilities":["fallback.demux"]})).unwrap()).unwrap();
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn planning_does_not_load_code_or_probe_sources() {
    let dir = Directory::new();
    dir.manifest("test.json", "does-not-exist.dll", std::env::consts::OS);
    let e = Engine::discover(&dir.0, 256 * 1024 * 1024).unwrap();
    assert_eq!(
        e.plan(&Request::playback("missing.mkv"))
            .unwrap()
            .graph
            .nodes[0]
            .config
            .component,
        "fallback.demux"
    );
    dir.manifest("duplicate.json", "other.dll", std::env::consts::OS);
    assert!(Engine::discover(&dir.0, 256 * 1024 * 1024).is_err());
}
#[test]
fn reject_traversal_oversize_and_old_abi() {
    let dir = Directory::new();
    dir.manifest("test.json", "../other.dll", std::env::consts::OS);
    assert!(Engine::discover(&dir.0, 256 * 1024 * 1024).is_err());
    dir.manifest("test.json", "other.dll", "unsupported-os");
    assert!(
        Engine::discover(&dir.0, 256 * 1024 * 1024)
            .unwrap()
            .registry()
            .candidates
            .is_empty()
    );
    for (major, minor) in [(1, 0), (2, u16::MAX)] {
        let value = json!({"id":"test", "abi_major":major, "abi_minor":minor,
            "os":std::env::consts::OS, "arch":std::env::consts::ARCH,
            "library":"other.dll", "capabilities":["fallback.demux"]});
        std::fs::write(dir.0.join("test.json"), serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            Engine::discover(&dir.0, 256 * 1024 * 1024)
                .unwrap()
                .registry()
                .candidates
                .is_empty()
        );
    }
    std::fs::write(dir.0.join("test.json"), vec![b' '; 65537]).unwrap();
    assert!(Engine::discover(&dir.0, 256 * 1024 * 1024).is_err());
}
