"""Architecture diagnostics are promises of the project check command."""
import unittest

from dev import ProjectContract, ROOT


class ArchitectureContract(ProjectContract):
    def setUp(self):
        super().setUp()
        self.manifest = (self.root / "Cargo.toml").read_text()
        (self.root / "Cargo.toml").write_text(self.manifest +
            '\n[package.metadata.snap]\nrole="application"\n')
        (self.root / "src/lib.rs").write_text('#![no_std]\n')

    def dependency(self, name, role, source='#![no_std]\n'):
        path = self.root / name
        (path / "src").mkdir(parents=True)
        (path / "Cargo.toml").write_text(
            f'[package]\nname="{name}"\nversion="0.0.0"\nedition="2024"\n'
            f'[package.metadata.snap]\nrole="{role}"\n')
        (path / "src/lib.rs").write_text(source)

    def test_portable_platform_edge_reports_target_kind_and_remedy(self):
        self.dependency("fixture-platform", "platform")
        (self.root / "Cargo.toml").write_text(self.manifest + '''
[package.metadata.snap]
role="application"
[features]
browser=["dep:fixture-platform"]
[target.'cfg(target_arch="wasm32")'.dependencies]
fixture-platform={path="fixture-platform",optional=true}
''')
        result = self.run_cli("check", "--structure-only")
        self.assertNotEqual(result.returncode, 0)
        for text in ["cli-fixture", "fixture-platform", "normal", "wasm32", "composition"]:
            self.assertIn(text, result.stderr)

    def test_new_workspace_portable_package_is_checked_automatically(self):
        self.dependency("extra", "core", 'pub fn host_only() { std::thread::yield_now(); }\n')
        (self.root / "Cargo.toml").write_text(self.manifest.replace('[workspace]',
            '[workspace]\nmembers=["extra"]') + '\n[package.metadata.snap]\nrole="application"\n')
        selected = self.run_cli("check", "--structure-only")
        self.assertEqual(selected.returncode, 0, selected.stderr)
        workspace = self.run_cli("check", "--structure-only", "--workspace")
        self.assertNotEqual(workspace.returncode, 0)
        self.assertIn("extra", workspace.stderr)
        self.assertIn("wasm32v1-none", workspace.stderr)

    def test_missing_roles_and_shared_dev_application_edges_are_errors(self):
        self.dependency("fixture-app", "application")
        (self.root / "Cargo.toml").write_text(self.manifest + '''
[package.metadata.snap]
role="core"
[dev-dependencies]
fixture-app={path="fixture-app"}
''')
        result = self.run_cli("check", "--structure-only")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("dev", result.stderr)
        self.assertIn("fixture-app", result.stderr)
        (self.root / "Cargo.toml").write_text(self.manifest)
        result = self.run_cli("check", "--structure-only")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("package.metadata.snap.role", result.stderr)


if __name__ == "__main__":
    (ROOT / ".tmp").mkdir(exist_ok=True)
    unittest.main()
