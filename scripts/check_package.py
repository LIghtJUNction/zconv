"""Audit the dependency and packaged-file contract using only Python's stdlib."""
import json
import pathlib
import subprocess
import tomllib


def main():
    manifest = tomllib.loads(pathlib.Path('Cargo.toml').read_text())
    package = manifest['package']
    assert package['name'] == 'zconv', 'unexpected crate name'
    assert package['rust-version'] == '1.99.1'
    assert not pathlib.Path('build.rs').exists(), 'build.rs is forbidden'
    sections = [manifest, *manifest.get('target', {}).values()]
    for section in sections:
        for name in ('dependencies', 'dev-dependencies', 'build-dependencies'):
            assert not section.get(name), f'forbidden {name}'
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--format-version', '1', '--offline', '--locked'], text=True))
    assert len(metadata['packages']) == 1, 'published crate dependency graph must have one node'
    assert not metadata['packages'][0]['dependencies']
    files = subprocess.check_output(['cargo', 'package', '--list', '--allow-dirty'], text=True).splitlines()
    for name in files:
        assert not name.startswith(('comparison/', 'fuzz/', '.github/')), name
        assert name != 'build.rs'
    core = pathlib.Path('src/lib.rs').read_text()
    assert '#![no_std]' in core
    assert '#![forbid(unsafe_code)]' in core
    for path in pathlib.Path('src').rglob('*.rs'):
        data = path.read_text()
        assert 'extern crate alloc' not in data, path
        assert 'extern crate std' not in data, path
    print('PASS: core-only, zero dependencies of every kind, no build script; package isolated')


if __name__ == '__main__':
    main()
