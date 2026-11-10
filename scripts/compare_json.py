"""Compare JSON kernels with the same harness, compiler and release settings.

Run from the repository root. Temporary crates have no dependencies. No source
file in the checkout is changed. Output files are benchmark measurements, not
correctness thresholds. Run without concurrent compilation or other benchmarks.
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', default='96fa6fc606e265d58b60a20073f2354f7564169e')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root = Path.cwd()
    args.output.mkdir(parents=True, exist_ok=True)
    baseline = subprocess.check_output(['git', 'show', f'{args.baseline}:src/json.rs'])
    with tempfile.TemporaryDirectory(prefix='zconv-json-') as temporary:
        executables = {}
        for name, kernel in [('before', baseline), ('after', (root / 'src/json.rs').read_bytes())]:
            directory = Path(temporary) / name
            (directory / 'src').mkdir(parents=True)
            (directory / 'benches').mkdir()
            (directory / 'Cargo.toml').write_text(
                '[package]\nname="zconv"\nversion="0.1.0"\nedition="2021"\n'
                '[workspace]\n[[bench]]\nname="json"\nharness=false\n'
                '[profile.bench]\nlto="thin"\ncodegen-units=1\n', encoding='utf-8')
            (directory / 'src/lib.rs').write_text(
                '#![no_std]\npub mod json;\npub mod types;\npub use types::*;\n', encoding='utf-8')
            (directory / 'src/json.rs').write_bytes(kernel)
            shutil.copyfile(root / 'src/types.rs', directory / 'src/types.rs')
            shutil.copyfile(root / 'benches/json.rs', directory / 'benches/json.rs')
            output = subprocess.check_output(
                ['cargo', 'bench', '--bench', 'json', '--no-run', '--message-format=json'],
                cwd=directory, text=True)
            messages = [json.loads(line) for line in output.splitlines()]
            executables[name] = next(item['executable'] for item in messages
                if item.get('reason') == 'compiler-artifact' and item.get('executable'))
        for number, order in enumerate([('before', 'after'), ('after', 'before')], start=1):
            for name in order:
                with (args.output / f'json-{name}-{number}.csv').open('w', encoding='utf-8') as output:
                    subprocess.run([executables[name], '--full'], stdout=output, check=True)
    print(f'Measurements written to {args.output}')


if __name__ == '__main__':
    main()
