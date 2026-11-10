"""Initialize the ignored libFuzzer corpus with committed regression fixtures.

Run from any directory. Existing mutations are retained. All seed input is
synthetic test data; no model credentials, network calls or user data are read.
"""
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / 'fuzz' / 'corpus'


def write(target, name, data):
    directory = CORPUS / target
    directory.mkdir(parents=True, exist_ok=True)
    (directory / ('seed-' + name)).write_bytes(data)


def main():
    fixtures = (ROOT / 'tests/protocol.rs').read_text(encoding='utf-8')
    arrays = {}
    for name in ('REQUESTS', 'RESPONSES'):
        block = re.search(r'const ' + name + r'.*?= \[(.*?)\];', fixtures, re.S)
        if block is None:
            raise ValueError(f'missing committed fixture array {name}')
        arrays[name] = re.findall(r'br#"(.*?)"#', block.group(1), re.S)
        if len(arrays[name]) != 6:
            raise ValueError(f'{name} must contain one fixture per protocol')
    for operation, values in arrays.items():
        selectors = (0, 1, 2) if operation == 'REQUESTS' else (6, 4, 5)
        for source, fixture in enumerate(values):
            payload = fixture.encode('utf-8')
            write('json', f'{operation}-{source}', payload)
            for target in range(6):
                for strategy in selectors:
                    write('convert', f'{operation}-{source}-{target}-{strategy}',
                          bytes([source, target, strategy]) + payload)
    for i, payload in enumerate([
        b'{"a":0,"\\u0061":1}', b'"\\ud83d\\ude00"',
        b'[0,-0,1e+9,1.25,null,true,false]', b'{"ignored":[{"x":"\\uD800"}]}',
        b'[' * 65 + b'0' + b']' * 65,
    ]):
        write('json', f'grammar-{i}', payload)
    chat = {'id': 'r', 'model': 'm', 'created': 1, 'choices': [
        {'index': 0, 'delta': {'content': 'hello 中 😀'}, 'finish_reason': 'stop'}],
        'usage': {'prompt_tokens': 2, 'completion_tokens': 1, 'total_tokens': 3}}
    wire = ('data: ' + json.dumps(chat, ensure_ascii=False, separators=(',', ':'))
            + '\r\n\r\ndata: [DONE]\r\n\r\n').encode('utf-8')
    for target in range(6):
        for strategy in range(3):
            for chunk in (1, 64):
                write('stream', f'chat-{target}-{strategy}-{chunk}',
                      bytes([1, target, strategy, chunk]) + wire)
    print('Initialized 17 JSON, 216 conversion and 36 stream seeds; existing mutations retained')


if __name__ == '__main__':
    main()
