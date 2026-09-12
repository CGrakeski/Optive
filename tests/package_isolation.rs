#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;

use optive::vm::{DepPackage, Vm};

struct TestDir(std::path::PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "optive-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn package(root: &std::path::Path, name: &str, source: &str) -> std::path::PathBuf {
    let path = root.join(name);
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("main.tive"), source).unwrap();
    path
}

fn bind(vm: &mut Vm, parent: &str, name: &str, id: &str, path: std::path::PathBuf) {
    vm.dep_map.insert(
        (parent.to_string(), name.to_string()),
        DepPackage {
            path,
            id: id.to_string(),
        },
    );
}

#[test]
fn diamond_dependencies_keep_different_versions_isolated() {
    let temp = TestDir::new("multi-version");
    let a = package(
        temp.path(),
        "a",
        "import c\nexport let version = c.version\n",
    );
    let b = package(
        temp.path(),
        "b",
        "import c\nexport let version = c.version\n",
    );
    let c1 = package(temp.path(), "c1", "export let version = \"v1\"\n");
    let c2 = package(temp.path(), "c2", "export let version = \"v2\"\n");

    let mut vm = Vm::new();
    bind(&mut vm, "__root__", "a", "a-id", a);
    bind(&mut vm, "__root__", "b", "b-id", b);
    bind(&mut vm, "a-id", "c", "c-v1-id", c1);
    bind(&mut vm, "b-id", "c", "c-v2-id", c2);

    let value = optive::run_source_in_vm(
        &mut vm,
        "import a\nimport b\n(a.version, b.version)\n",
        "<script>",
    )
    .unwrap();
    assert_eq!(value.display_string(), "(\"v1\", \"v2\")");
    assert_eq!(vm.module_cache.len(), 4);
}

#[test]
fn diamond_dependencies_share_an_identical_resolved_package() {
    let temp = TestDir::new("shared-version");
    let a = package(
        temp.path(),
        "a",
        "import c\nexport let version = c.version\n",
    );
    let b = package(
        temp.path(),
        "b",
        "import c\nexport let version = c.version\n",
    );
    let c = package(temp.path(), "c", "export let version = \"shared\"\n");

    let mut vm = Vm::new();
    bind(&mut vm, "__root__", "a", "a-id", a);
    bind(&mut vm, "__root__", "b", "b-id", b);
    bind(&mut vm, "a-id", "c", "c-id", c.clone());
    bind(&mut vm, "b-id", "c", "c-id", c);

    let value = optive::run_source_in_vm(
        &mut vm,
        "import a\nimport b\n(a.version, b.version)\n",
        "<script>",
    )
    .unwrap();
    assert_eq!(value.display_string(), "(\"shared\", \"shared\")");
    assert_eq!(vm.module_cache.len(), 3);
}

#[test]
fn trusted_dependency_still_cannot_write_its_source() {
    let temp = TestDir::new("trusted-read-only");
    let mut host = optive::caps::Capabilities::full();
    host.dep_grant.trust_all = true;
    let dep = host.restrict_for_dependency(temp.path(), "dep-id");
    let err = dep
        .write("test write", temp.path().join("changed.tive"), "no")
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("installed package source is immutable"));
}

#[test]
fn package_state_directories_are_separated_by_exact_package_id() {
    let a = optive::caps::package_state_dir("same-v1", "data");
    let b = optive::caps::package_state_dir("same-v2", "data");
    let cache = optive::caps::package_state_dir("same-v1", "cache");
    assert_ne!(a, b);
    assert_ne!(a, cache);
}
