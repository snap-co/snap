"""Architecture diagnostics are promises of the project check command."""
import unittest

from dev import ProjectContract, ROOT


class ArchitectureContract(ProjectContract):
    def test_contracts_cannot_select_even_portable_providers(self):
        self.dependency("provider", "core")
        (self.root / "Cargo.toml").write_text(self.manifest + '''
[package.metadata.snap]
role="contract"
[dependencies]
provider={path="provider"}
''')
        result = self.run_cli("check", "--structure-only")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("keep contracts independent of providers", result.stderr)
        self.assertIn("normal", result.stderr)
        provider = self.root / "provider/Cargo.toml"
        provider.write_text(provider.read_text().replace('role="core"', 'role="contract"'))
        result = self.run_cli("check", "--structure-only")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_explicit_rlib_is_a_portable_library(self):
        with (self.root / "Cargo.toml").open("a") as manifest:
            manifest.write('\n[lib]\ncrate-type=["rlib"]\n')
        result = self.run_cli("check", "--structure-only")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_portable_path_dependency_features_are_validated_in_its_workspace(self):
        self.dependency("external", "core")
        self.dependency("external/platform", "platform")
        # A separate workspace's optional dependency is enabled by its own
        # all-feature portability check, not the consuming app's metadata call.
        (self.root / "external/platform/Cargo.toml").write_text(
            '[package]\nname="fixture-platform"\nversion="0.0.0"\nedition="2024"\n'
            '[package.metadata.snap]\nrole="platform"\n')
        with (self.root / "external/Cargo.toml").open("a") as manifest:
            manifest.write('''
[workspace]
[features]
host=["dep:fixture-platform"]
[dependencies]
fixture-platform={path="platform",optional=true}
''')
        (self.root / "Cargo.toml").write_text(self.manifest.replace('[workspace]',
            '[workspace]\nexclude=["external"]') + '''
[package.metadata.snap]
role="application"
[dependencies]
external={path="external"}
''')
        external = self.root / "external/Cargo.toml"
        text = external.read_text()
        for header in ["dependencies", 'target.\'cfg(target_os="none")\'.dependencies']:
            with self.subTest(header=header):
                external.write_text(text.replace("[dependencies]", f"[{header}]"))
                result = self.run_cli("check", "--structure-only")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("fixture-platform", result.stderr)
                self.assertIn("normal", result.stderr)
                self.assertIn("composition", result.stderr)

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
