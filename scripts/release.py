"""Version and registry gates. Input comes from environment, never shell interpolation."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from urllib.error import HTTPError
from urllib.request import Request, urlopen

SEMVER = re.compile(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\Z')
MAX_COMPONENT = 2**32 - 1


def version_tuple(value):
    match = SEMVER.fullmatch(value)
    if not match:
        raise ValueError('version must be stable SemVer: MAJOR.MINOR.PATCH')
    result = tuple(map(int, match.groups()))
    if any(v > MAX_COMPONENT for v in result):
        raise ValueError('SemVer component exceeds supported range')
    return result


def select_version(current, bump, explicit='', tag=''):
    old = version_tuple(current)
    if tag:
        if tag != 'v' + current:
            raise ValueError('release tag does not match Cargo.toml')
        if explicit and explicit != current:
            raise ValueError('explicit version conflicts with tag')
        return current
    if explicit:
        if bump != 'none':
            raise ValueError('explicit version requires bump=none')
        if version_tuple(explicit) < old:
            raise ValueError('version must not decrease')
        return explicit
    if bump not in ('none', 'patch', 'minor', 'major'):
        raise ValueError('unknown bump mode')
    major, minor, patch = old
    if bump == 'major':
        major, minor, patch = major + 1, 0, 0
    elif bump == 'minor':
        minor, patch = minor + 1, 0
    elif bump == 'patch':
        patch += 1
    result = f'{major}.{minor}.{patch}'
    version_tuple(result)
    return result


def manifest():
    return tomllib.loads(Path('Cargo.toml').read_text())['package']


def prepare():
    package = manifest()
    chosen = select_version(package['version'], os.getenv('BUMP', 'none'),
                            os.getenv('VERSION', ''), os.getenv('RELEASE_TAG', ''))
    if chosen != package['version']:
        path = Path('Cargo.toml')
        data, count = re.subn(r'(?m)^version = "' + re.escape(package['version']) + r'"$',
                             f'version = "{chosen}"', path.read_text(), count=1)
        if count != 1:
            raise ValueError('expected one package version field')
        path.write_text(data)
        # MSRV writes lockfile v3; newer cargo would create a lockfile unusable by 1.74.
        subprocess.run(['cargo', '+1.74.0', 'generate-lockfile', '--offline'], check=True)
    output = os.getenv('GITHUB_OUTPUT')
    if output:
        with open(output, 'a', encoding='utf-8') as file:
            file.write(f'version={chosen}\n')
    print(f'zconv {chosen}')


def get_json(url):
    request = Request(url, headers={'User-Agent': 'zconv-release (https://github.com/LIghtJUNction/zconv)'})
    with urlopen(request, timeout=30) as response:
        return json.load(response)


def registry():
    package = manifest()
    name, version = package['name'], package['version']
    if name != 'zconv':
        raise ValueError('crate naming must be changed coherently before release')
    api = f'https://crates.io/api/v1/crates/{name}'
    try:
        data = get_json(api)
    except HTTPError as error:
        if error.code != 404:
            raise
        print('Registry name is currently unoccupied; first publication may proceed')
        return
    repository = data['crate'].get('repository', '').rstrip('/').removesuffix('.git')
    if repository.lower() != 'https://github.com/lightjunction/zconv':
        raise ValueError('registry name is occupied by a different repository; stop and rename coherently')
    if any(item['num'] == version for item in data['versions']):
        raise ValueError(f'{name} {version} already exists; select a new version')
    owners = get_json(api + '/owners')['users']
    if not any(user.get('login', '').lower() == 'lightjunction' for user in owners):
        raise ValueError('expected registry owner is absent')
    print('Registry owner, repository and unpublished version verified')


if __name__ == '__main__':
    if len(sys.argv) != 2 or sys.argv[1] not in ('prepare', 'registry'):
        raise SystemExit('usage: release.py prepare|registry')
    globals()[sys.argv[1]]()
