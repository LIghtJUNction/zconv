from contextlib import chdir, redirect_stdout
from io import StringIO
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

import release
from release import select_version, update_path_lock


class ReleaseTests(unittest.TestCase):
    def test_bumps(self):
        for mode, expected in [('none', '1.2.3'), ('patch', '1.2.4'), ('minor', '1.3.0'), ('major', '2.0.0')]:
            self.assertEqual(select_version('1.2.3', mode), expected)

    def test_injection_and_invalid_semver(self):
        for value in ['01.2.3', '1.2', 'v1.2.3', '1.2.3\n', '1.2.3;echo bad', '$(id)', '1.2.3-rc.1']:
            with self.assertRaises(ValueError):
                select_version('1.2.3', 'none', value)

    def test_conflicting_inputs_and_downgrade(self):
        for args in [('1.2.3', 'patch', '1.2.4'), ('1.2.3', 'none', '1.2.2'), ('1.2.3', 'none', '', 'v1.2.4')]:
            with self.assertRaises(ValueError):
                select_version(*args)

    def test_tag_is_exact(self):
        self.assertEqual(select_version('0.1.0', 'none', '', 'v0.1.0'), '0.1.0')


class PrepareTests(unittest.TestCase):
    LOCK = '''# Preserve the lockfile version and registry resolution.
version = 4

[[package]]
name = "oracle"
version = "1.2.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "unchanged"

[[package]]
name = "zconv"
version = "1.2.3"
'''

    def test_path_lock_update_retains_registry_resolution(self):
        updated = update_path_lock(self.LOCK, 'zconv', '1.2.3', '1.2.4')
        before, after = tomllib.loads(self.LOCK), tomllib.loads(updated)
        self.assertEqual(after['version'], 4)
        self.assertEqual(before['package'][0], after['package'][0])
        self.assertEqual(after['package'][1], {'name': 'zconv', 'version': '1.2.4'})

    def test_path_lock_rejects_missing_stale_and_duplicate_stanzas(self):
        for data in (self.LOCK.replace('name = "zconv"', 'name = "different"'),
                     self.LOCK.replace('name = "zconv"\nversion = "1.2.3"',
                                       'name = "zconv"\nversion = "1.0.0"'),
                     self.LOCK + '\n[[package]]\nname = "zconv"\nversion = "1.2.3"\n'):
            with self.assertRaises(ValueError):
                update_path_lock(data, 'zconv', '1.2.3', '1.2.4')

    def test_prepare_bumps_every_local_lock_then_is_idempotent(self):
        with tempfile.TemporaryDirectory() as directory, chdir(directory):
            Path('Cargo.toml').write_text('[package]\nname = "zconv"\nversion = "1.2.3"\n')
            for project in ('comparison', 'fuzz'):
                Path(project).mkdir()
                Path(project, 'Cargo.lock').write_text(self.LOCK)
            environment = {'BUMP': 'patch', 'VERSION': '', 'RELEASE_TAG': '',
                           'GITHUB_OUTPUT': str(Path(directory, 'output'))}
            with patch.dict(os.environ, environment), patch.object(release.subprocess, 'run') as cargo:
                with redirect_stdout(StringIO()):
                    release.prepare()
                cargo.assert_called_once_with(
                    ['cargo', '+1.74.0', 'generate-lockfile', '--offline'], check=True)
            self.assertEqual(release.manifest()['version'], '1.2.4')
            expected = update_path_lock(self.LOCK, 'zconv', '1.2.3', '1.2.4')
            for project in ('comparison', 'fuzz'):
                self.assertEqual(Path(project, 'Cargo.lock').read_text(), expected)
            self.assertEqual(Path('output').read_text(), 'version=1.2.4\n')
            with patch.dict(os.environ, {**environment, 'BUMP': 'none'}), \
                    patch.object(release.subprocess, 'run') as cargo, redirect_stdout(StringIO()):
                release.prepare()
                cargo.assert_not_called()
            self.assertEqual(Path('comparison/Cargo.lock').read_text(), expected)


class GitPreflightTests(unittest.TestCase):
    def git(self, *args):
        return subprocess.check_output(['git', *args], text=True, stderr=subprocess.PIPE).strip()

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.enterContext(chdir(self.directory.name))
        self.git('init', '--bare', 'remote.git')
        Path('repo').mkdir()
        self.enterContext(chdir('repo'))
        self.git('init', '-b', 'main')
        self.git('config', 'user.name', 'Release test')
        self.git('config', 'user.email', 'test@example.invalid')
        Path('Cargo.toml').write_text('[package]\nname = "zconv"\nversion = "1.2.3"\n')
        self.git('add', 'Cargo.toml')
        self.git('commit', '-m', 'initial')
        self.git('remote', 'add', 'origin', '../remote.git')
        self.git('push', 'origin', 'main')
        self.head = self.git('rev-parse', 'HEAD')
        self.enterContext(patch.dict(os.environ, {
            'GITHUB_SHA': self.head, 'GITHUB_EVENT_NAME': 'workflow_dispatch'}))
        self.enterContext(redirect_stdout(StringIO()))

    def test_current_commit_and_matching_tag_are_valid(self):
        release.git_preflight()
        self.git('tag', '-a', 'v1.2.3', '-m', 'release')
        self.git('push', 'origin', 'v1.2.3')
        with patch.dict(os.environ, {'GITHUB_EVENT_NAME': 'push'}):
            release.git_preflight()

    def test_existing_tag_must_include_the_prepared_version(self):
        self.git('tag', 'v1.2.4')
        Path('Cargo.toml').write_text('[package]\nname = "zconv"\nversion = "1.2.4"\n')
        with self.assertRaisesRegex(ValueError, 'prepared version changes'):
            release.git_preflight()

    def test_existing_tag_cannot_point_to_different_source(self):
        self.git('tag', 'v1.2.3')
        Path('change').write_text('new source')
        self.git('add', 'change')
        self.git('commit', '-m', 'change')
        self.git('push', 'origin', 'main')
        with patch.dict(os.environ, {'GITHUB_SHA': self.git('rev-parse', 'HEAD')}):
            with self.assertRaisesRegex(ValueError, 'different commit'):
                release.git_preflight()

    def test_manual_release_rejects_a_branch_advanced_during_verification(self):
        Path('change').write_text('new source')
        self.git('add', 'change')
        self.git('commit', '-m', 'change')
        self.git('push', 'origin', 'main')
        self.git('checkout', '--detach', self.head)
        with self.assertRaisesRegex(ValueError, 'main advanced'):
            release.git_preflight()


class RegistryTests(unittest.TestCase):
    def setUp(self):
        self.enterContext(patch.object(release, 'manifest', return_value={'name': 'zconv', 'version': '1.2.3'}))
        self.enterContext(redirect_stdout(StringIO()))

    def test_unoccupied_name_and_confirmed_owner(self):
        with patch.object(release, 'get_json', side_effect=HTTPError('url', 404, 'missing', {}, None)):
            release.registry()
        data = {'crate': {'repository': 'https://github.com/LIghtJUNction/zconv.git'}, 'versions': []}
        with patch.object(release, 'get_json', side_effect=[data, {'users': [{'login': 'LIghtJUNction'}]}]):
            release.registry()

    def test_registry_failures_do_not_look_like_an_unoccupied_name(self):
        with patch.object(release, 'get_json', side_effect=HTTPError('url', 503, 'unavailable', {}, None)):
            with self.assertRaises(HTTPError):
                release.registry()
        for repository in (None, 'https://github.com/other/zconv'):
            with patch.object(release, 'get_json', return_value={'crate': {'repository': repository}}):
                with self.assertRaisesRegex(ValueError, 'different repository'):
                    release.registry()

    def test_published_version_and_missing_owner_are_rejected(self):
        data = {'crate': {'repository': 'https://github.com/LIghtJUNction/zconv'},
                'versions': [{'num': '1.2.3'}]}
        with patch.object(release, 'get_json', return_value=data):
            with self.assertRaisesRegex(ValueError, 'already exists'):
                release.registry()
        data['versions'] = []
        with patch.object(release, 'get_json', side_effect=[data, {'users': []}]):
            with self.assertRaisesRegex(ValueError, 'owner is absent'):
                release.registry()


if __name__ == '__main__':
    unittest.main()
